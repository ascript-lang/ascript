# AScript Baseline JIT (Cranelift) — Design (JIT)

- **Status:** Draft for review — **EXPLORATORY, DEFERRED. Not scheduled. Do not implement.**
- **Date:** 2026-06-08
- **Code:** JIT (the single *deferred* item of the Serious Language campaign — see `goal.md`)
- **Depends on (HARD gate):** **NUM merged** *and* **VAL merged** *and* **profiling evidence**
  (see §1.1). All three are preconditions; absent any one, this design must not be implemented.
- **Depended on by:** nothing.
- **Engines:** adds a **fourth** execution tier (JIT) that must be byte-identical to the existing
  three (tree-walker == specialized-VM == generic-VM == **JIT**). The tree-walker remains the
  permanent oracle; the bytecode interpreter remains tier-0 and the deopt target.
- **Breaking:** **no.** Runtime-only, in-memory. No `.aso` format change, no opcode change, no
  surface-syntax change.

---

## 0. Read this first — the deferral is the design

This spec exists to **capture a considered design and surface its risks before any code is
written**, exactly so that the eventual go/no-go decision is made against a concrete plan rather
than an assumption. It is **not** a work order. Three things are true and stated up front, and
repeated wherever they bear on a decision:

1. **A JIT is the *only* sanctioned deferral in the campaign** (`goal.md` §"Deferred"), alongside
   a possible future GC rework. It is gated on **evidence, not schedule**.
2. **It must not be implemented before NUM *and* VAL are merged** (§1.1). A JIT over the current
   value model — every number a boxed `f64` (`Value::Number(f64)`, `src/value.rs`) and a fat
   `Value` enum — has a **low ceiling**: there is little native code can do that the interpreter's
   adaptive `f64` fast path (`src/vm/adapt.rs`) does not already do, and every operation still
   chases an `Rc`/`Cc` pointer. The integer fast paths NUM introduces are *why* a JIT can pay off;
   the **unboxed/inlined scalars VAL introduces** (a `Value` ≤16 bytes — 8-byte NaN-box *or* the
   sanctioned 16-byte niche fallback, VAL §3.2/§3.3) are the *other* precondition. Without both,
   this is negative-ROI work.
3. **It must not be implemented before profiling proves interpreter dispatch is the bottleneck**
   on real workloads (§1.1). If the bottleneck is allocation, GC, or I/O, a JIT does not help and
   VAL/GC work matters more.

If the reader takes one thing: **the correctness gate (§5, the four/five-way differential) is the
reason this is gated, not the speed.** A JIT that diverges from the tree-walker is a bug in the
JIT, and chasing byte-identity across a native code generator is the single largest risk here.

## 1. Summary & motivation

The default AScript engine is an async bytecode VM: a CST front-end → resolver → bytecode compiler
→ a `Chunk` (`src/vm/chunk.rs`) → an instruction-dispatch loop (`Vm::run_loop`, `src/vm/run.rs:581`).
The loop already does real specialization work — PEP-659-style **adaptive arithmetic**
(`src/vm/adapt.rs`, `ArithKind::{Number,Decimal,ConcatStr}`) and **polymorphic inline caches** for
property/method access (`src/vm/ic.rs`). Those close much of the gap to native code *for free*,
which is precisely why a JIT is deferred rather than scheduled: the cheap wins are already taken.

A **baseline JIT** would compile a hot `FnProto` (`src/vm/chunk.rs:335` — the natural unit of
compilation) from its bytecode to native machine code once it has executed often enough, then
dispatch subsequent calls to the compiled version. The interpreter stays as the always-available
**tier-0** and the **deopt target**: any time the compiled code's specialization assumptions fail
(a guard miss), execution falls back to the interpreter at a well-defined bytecode offset.

The realistic payoff is on **numeric- and loop-heavy code** — the workloads where per-instruction
dispatch overhead and bytecode operand decoding dominate, and where NUM's unboxed integers let
native code emit a real machine `add`/`imul` with an overflow check instead of a boxed-`f64` round
trip. The realistic *non*-payoff is **allocation-bound code**: a JIT cannot make `Rc`/`Cc`
refcount traffic or cycle-GC pauses go away — VAL (allocation discipline, escape analysis) is the
lever there, not the JIT. This honesty is load-bearing; see §7.

### 1.1 The deferral preconditions (the gate — all three required)

| # | Precondition | Why | How verified |
|---|---|---|---|
| 1 | **NUM merged** | Integer fast paths are the reason native codegen pays off; without unboxed `int`, the JIT's arithmetic is the same boxed `f64` the interpreter already specializes. | `goal.md` execution order; `Value::Int` exists in `src/value.rs`. |
| 2 | **VAL merged** | A compact `Value` whose **scalars (`int`/`float`) are unboxed/inlined** is what lets native code keep operands in registers and test tags cheaply; over today's fat 32-byte enum + `Rc` the codegen has nothing to bite on. | VAL's compact representation is live: **scalars are unboxed/inlined and `Value` is ≤16 bytes** — the property *both* VAL outcomes deliver (8-byte NaN-box *or* the sanctioned 16-byte niche fallback, VAL §3.2/§3.3). Full NaN-boxing is a bonus, not a precondition. |
| 3 | **Profiling evidence** | A JIT is justified by measurement, not assumption (`goal.md` pillar 4). If dispatch is not the measured bottleneck on real workloads, the JIT is the wrong investment. | A profiling report (sibling to `bench/PROFILING_RESULTS.md`) shows interpreter **dispatch** — not allocation/GC/I/O — dominating a representative CPU-bound corpus, and estimates the headroom a baseline JIT could recover. |

Until **all three** hold, this document is reference material only. The go/no-go is a campaign-level
decision recorded against this spec, exactly as `goal.md` requires ("explicitly deferred with a
recorded, owner-noted justification").

## 2. The tiering model

A two-tier model: **tier-0 = the existing bytecode interpreter**; **tier-1 = the Cranelift baseline
JIT**. There is deliberately no tier-2 (an optimizing JIT) — that is a separate, even-further-out
question and a documented non-goal here (§8).

### 2.1 Counters and the hot threshold

Tier-up is triggered by **per-`FnProto` execution counting**, mirroring how `adapt.rs` already
warms a site over `WARMUP_THRESHOLD` (`src/vm/adapt.rs:44`) before specializing — the JIT is the
same idea one level up (whole function instead of one arith site):

- Each `FnProto` gets a monotonic **call counter** plus a **loop-backedge counter** (a hot function
  is one that is *called* a lot **or** spins a hot loop). The backedge count is incremented at
  `Op::Loop` (`src/vm/run.rs:1418`, the backward-jump arm); the call count at frame push
  (`src/vm/run.rs:1253`, the `fiber.frames.push` site). **Both increment sites sit in the hottest
  interpreter paths**, so their cost when the JIT is off is non-negotiable — see "Where the
  counters live" below.
- When either counter crosses `JIT_THRESHOLD` (a tunable; V8 Sparkplug tiers very eagerly,
  LuaJIT/HotSpot far less — pick empirically against the profiling corpus, start conservative), the
  `FnProto` is **enqueued for compilation**. State machine, per `FnProto`:
  `Cold → Warming(count) → Queued → Compiled(code_ptr) → (Blacklisted on bail)`.
- **Where the counters live (zero-cost-when-off is a hard requirement):**
  - **Compiled OUT when the JIT is off.** Both counters and their two increment sites
    (`run.rs:1253`, `run.rs:1418`) are `#[cfg(feature = "jit")]`-gated. With the (default-off) `jit`
    feature disabled, the increments do not exist in the compiled interpreter at all — the hot
    paths are byte-for-byte the pre-JIT loop. This is the SP8 "predictably-not-taken vs. not-there"
    discipline applied to the two hottest sites in the engine.
  - **A CHEAP home when on.** When `jit` is enabled, each counter is a plain `Cell<u32>`
    **co-located on `FnProto`** (the call counter) and reached through the live frame's
    `closure.proto` at the `Op::Loop` backedge (the backedge counter) — a direct field bump, **NOT
    a `RefCell<HashMap>` lookup per backedge** (a per-iteration hash probe in the loop back-edge
    would itself be the regression we are trying to avoid). Following the `adapt.rs` precedent the
    counters are still NOT inline in the immutable, shared, byte-identical `Chunk.code` (the
    disassembler, goldens, and the differential oracle depend on byte-identical bytecode — see the
    `adapt.rs` module doc, "Why a side map, not in-place quickening"); a `Cell<u32>` field beside
    the `Chunk` on `FnProto` is a side datum, not an operand, so bytecode and disassembly stay
    **byte-identical**, zero new inline operand, no `.aso` change. (The richer per-`FnProto` JIT
    *state* — `Cold/Warming/Queued/Compiled/Blacklisted` + the installed `code_ptr` — lives on the
    per-isolate JIT side table keyed by `FnProto` identity, §6; only the two hot counters are
    co-located for cheapness.)
  - **Benchmarked.** A required deliverable is a microbenchmark proving **no steady-state regression
    with `jit` enabled-but-cold** (counters incrementing, nothing compiled) against the JIT-off
    build, on the call-heavy and loop-heavy corpus — the same "no steady-state regression" bar Gate
    12 sets for NUM's `ArithKind::Int` path and SP8's instrumentation seams. If the cold-counter
    bump shows up in steady state, the home is wrong; fix the home, do not relax the gate.

### 2.2 Tier-up (compilation)

For v1, compile **synchronously, lazily, on the threshold-crossing call** (simplest, fully
deterministic ordering). The crossing call detects `Queued`, runs the Cranelift translator
(§3) over the `FnProto`'s bytecode to produce a native function, installs the `code_ptr` into the
`FnProto`'s JIT slot, and dispatches the *current* call to it. An **async/background-thread
compile** (V8-style, compile off the hot path) is a deliberate **future refinement, not v1** —
it interacts with the share-nothing `!Send` model (the compile would have to be `Send`-clean over a
snapshot of the `FnProto`, and Cranelift's `JITModule` is not trivially shareable) and adds
nondeterminism to *when* a function is compiled, which complicates the differential (§5). Keep v1
synchronous; revisit only if compile latency shows up in the profiling.

### 2.3 The interpreter as tier-0 and the deopt target

The bytecode interpreter is **never removed and always reachable**:

- A `FnProto` that never gets hot **never compiles** — it runs on the interpreter forever, exactly
  as today. Cold/one-shot code pays zero JIT cost.
- A compiled `FnProto` that **bails** (hits an unsupported op mid-translation, §3.4) or **deopts**
  too often (churns its guards) is **blacklisted** and runs on the interpreter forever after — the
  interpreter is the correctness floor.
- **Deopt always lands in the interpreter** at a defined bytecode offset (§4). There is no "deopt
  to a less-optimized JIT tier"; tier-0 is the only fallback. This keeps the deopt target a single,
  already-proven, byte-identical engine.

The dispatch decision at a call site (`Op::Call` / `Op::CallMethod`, `src/vm/run.rs`) becomes: *if
the callee `FnProto` has an installed `code_ptr`, enter native code; else push an interpreter frame
as today.* The native entry and the interpreter frame push **must apply the identical
arity/contract/rest checks** (`check_call_args` via `proto.params`, `src/vm/chunk.rs:358`) so the
two tiers bind arguments and panic byte-identically — the JIT does not get its own calling
convention for arg checking; it calls the same shared checker.

## 3. Codegen & type feedback

### 3.1 Cranelift, not LLVM

The baseline JIT uses **Cranelift** (the Wasmtime/Bytecodealliance code generator), not LLVM:

- **Fast compile** — Cranelift is designed for JIT/baseline use where compile time is on the hot
  path; LLVM's compile latency (often 10–100× Cranelift's) would dominate at our tier-up
  granularity and defeat the point of a *baseline* tier.
- **Good-enough codegen** — a baseline JIT targets "much better than an interpreter loop," not
  "peak optimizing throughput." Cranelift clears that bar comfortably.
- **Pure Rust, no native toolchain** — `cranelift-jit` + `cranelift-frontend` integrate as ordinary
  crates behind a Cargo **feature flag** (§6, default-off initially), consistent with the existing
  pure-Rust dependency posture (`stacker`, `gcmodule` in `Cargo.toml`). No LLVM build dependency,
  no system linker at runtime.

The translator walks the `FnProto`'s bytecode once and emits Cranelift IR (CLIF) per opcode,
reusing the existing `Op::operand_width` (`src/vm/opcode.rs:623`) decode discipline. Stack-machine
bytecode lowers naturally to an SSA builder (operands become CLIF values; the operand stack becomes
a compile-time value stack the translator tracks, not a runtime stack — the classic "abstract
interpretation of the bytecode" baseline-JIT shape).

### 3.2 Consuming the existing type feedback

The interpreter already collects exactly the type feedback a JIT needs. **The JIT reads it; it does
not invent a new profiling mechanism.**

- **Adaptive arithmetic (`src/vm/adapt.rs`).** An arith site's `ArithCache::Specialized { kind }`
  records that the site has stably seen one `ArithKind`. Post-NUM this gains **`ArithKind::Int`,
  which coexists with the existing `ArithKind::Number` (now meaning `float`/`f64`)** — NUM adds the
  `Int` variant alongside `Number`, it does not replace it (NUM spec §7, `src/vm/adapt.rs`). The JIT
  translator reads the `arith_cache(op_off)` (`src/vm/chunk.rs:607`) for each arithmetic op: a site
  specialized to `Int` emits a **native integer add/sub/mul with an overflow check** (§3.3) guarded
  by a tag/`is-int` test on the two operands; a site specialized to `Number` (float) emits native
  `f64` ops behind a float guard;
  an unspecialized or polymorphic site emits a **call to the shared generic `apply_binop`** (the
  same function the interpreter falls through to — never a divergent reimplementation).
- **Inline caches (`src/vm/ic.rs`).** A `GET_PROP`/`SET_PROP` site that is `InlineCache::Mono {
  shape, index }` lets the JIT emit a **shape-guarded direct slot load**: compare the receiver's
  `shape_id` against the cached `shape`, and on a hit do a direct `values.get_index(index)` — the
  exact fast path `src/vm/run.rs` already takes, now inlined as native code. `Poly` sites emit a
  small guarded scan (≤ `POLY_MAX`); `Mega` sites emit a call to the generic member read. Method
  dispatch (`MethodCache::Mono`, `src/vm/ic.rs:147`) similarly lowers to a class-identity guard +
  direct compiled-closure entry.
- **Global cache (`src/vm/adapt.rs`, `GlobalCache`).** A `GET_GLOBAL` that resolved to a builtin
  (`Cached`) or a stable user-global index (`IndexBound`) lowers to a guarded direct read; the
  `struct_gen`/`version` guard becomes a native compare against the live counter.

The JIT therefore **specializes for what the function actually ran with**, exactly like the
interpreter, and degrades to the same shared generic helpers when feedback is absent or
polymorphic. Every "fall through to generic" in the interpreter becomes a "call the same generic
helper" in the JIT — that symmetry is what makes byte-identity achievable.

### 3.3 Overflow & wrapping semantics (NUM)

NUM makes integer `+ - * **`, unary `-`, and `<<` **checked** (trap on i64 overflow with a
recoverable Tier-2 panic), with explicit `+% -% *%` two's-complement **wrapping** operators as the
escape hatch (NUM §3.2). **The JIT must preserve these semantics exactly:**

- A specialized-`Int` checked op emits Cranelift's **overflow-checking integer instructions**
  (`iadd`/`imul` with the overflow flag tested, e.g. via `*_with_overflow`-style lowering or an
  explicit flag check) and, on overflow, **branches to a call into the SHARED panic-raising path**
  (mechanism (a), §4.1) so the message (`integer overflow in '<op>'`) and span are byte-identical to
  the interpreter's. It emits a **conditional branch → shared-helper call**, **NOT** a CLIF `trap`
  instruction (a raw `trap` would abort the process / surface a Cranelift trap code, not produce the
  recoverable Tier-2 `[value, err]` panic the interpreter raises — that would diverge). It must also
  **not** emit a silently-wrapping native `add` for a checked op — that would be a silent-wraparound
  bug, forbidden by pillar 1 (`goal.md`) and Gate 6.
- A wrapping op (`+% -% *%`) emits the plain wrapping native instruction (no overflow check) —
  matching NUM's wrapping semantics.
- `int / 0`, `% 0`, shift-amount ≥ 64 / < 0, and the mixed-type promotion rules (NUM §3.2) are
  conditions the JIT either **guards and calls the generic path** for, or **branches to the same
  shared panic-raising path** the interpreter uses (again a conditional branch → shared-helper call,
  never a CLIF `trap` and never silent UB). When in doubt, the translator emits a call to the shared
  helper; correctness beats coverage for a baseline tier.

### 3.4 Guards, bailout, and "compile only what you understand"

A baseline JIT does **not** need to compile every opcode. The translator is allowed to **bail**:
when it reaches an op it does not (yet) lower natively — `Await`, `Yield`, `MakeGenerator`,
`Import`, the destructuring/match family, anything touching native resources — it can either (a)
emit a **call back into the interpreter for the rest of the function** (a clean tier-0 handoff), or
(b) **abandon compilation entirely** and blacklist the `FnProto` (it stays on the interpreter).

For v1, prefer **(b) whole-function bail** for any unsupported op: compile only functions whose
bodies are entirely lowerable (straight-line + branches + loops + calls + arithmetic + shaped
property access + the cached fast paths). This keeps the translator small and the deopt story
simple (§4). Async/generator functions (`is_async`/`is_generator` on `FnProto`,
`src/vm/chunk.rs:339`) are **out of scope for v1** — they suspend across `.await`, which a baseline
native frame cannot model without continuation support the engine deliberately does not have (the
M17 async non-goals, `CLAUDE.md`). They run on the interpreter, full stop.

## 4. Deopt — the hard part, designed honestly

**Deopt is the single hardest correctness problem in a JIT**, and the reason this spec is
conservative. When a guard in compiled code fails (an `Int` site sees a float; a `Mono` shape guard
misses; an overflow trap fires), execution must resume **in the interpreter** producing **exactly**
the result the interpreter would have produced had the function never been compiled.

### 4.1 v1 scope: function-entry deopt only — NO mid-function OSR

The classic hard case is **on-stack replacement (OSR)**: reconstructing a full interpreter frame
(operand stack, locals, ip) from the *middle* of an optimized native frame, at an arbitrary guard
site, so the interpreter can pick up mid-function. OSR is where deopt bugs live (the literature —
Hölzle/Chambers/Ungar's Self deopt work, and every production VM since — treats it as the subtle
part). **v1 explicitly does not do mid-function OSR.**

Instead, v1 deopt is built from **two independent mechanisms that must be kept conceptually
separate** — the spec was previously wrong to entangle them, attaching a "no observable effect yet"
precondition to a mechanism that does not need one:

- **Mechanism (a) — per-op guard-miss → shared generic helper, continue in native code.** This is
  the interpreter's *own* per-op deopt lifted into the JIT. When a fast-path guard inside a compiled
  body misses (an `Int` site sees a float, a `Mono` shape guard misses), the compiled code **calls
  the SAME shared generic helper the interpreter itself falls through to** — for arithmetic that is
  `crate::interp::apply_binop` (the interpreter's literal guard-miss path: `src/vm/run.rs:3798-3803`,
  where the `_ => { … set_arith_cache(deopt); apply_binop(op, a, b, span) }` arm runs after a kind
  mismatch) — and **continues executing in native code** with that canonical result. This mechanism
  is **SOUND regardless of any prior side effect in the frame**: the helper produces exactly the
  value-or-panic the interpreter would, and control resumes at the same compile-time stack position.
  It carries **no entry-window precondition.**
- **Mechanism (b) — entry re-dispatch of the whole call to the interpreter.** When a *non-fast-path*
  op is reached (the whole-function bail story, §3.4) the entry path may instead **re-execute the
  entire call on tier-0** with the original arguments. This is byte-identical *only because nothing
  has happened yet* — it is therefore **sound ONLY in the no-prior-side-effect entry window.** It is
  an **entry-only** mechanism; it is never used mid-frame.

**The load-bearing invariant (state it explicitly):** *after the FIRST observable effect in a frame,
every remaining guard lowers to mechanism (a) only; mechanism (b) is entry-only.* The translator
therefore tracks a single "has the frame produced an observable effect yet?" bit as it lowers
opcodes: while still false, an unsupported/awkward op may resolve by entry re-dispatch (b); once
true, every fast-path guard must lower to a mechanism-(a) inline generic-helper call (which itself
produces the canonical result/panic) — never to a whole-frame re-dispatch.

**Supporting invariant (stack discipline):** for mechanism (a) to be a transparent substitution, the
guarded fast path and the generic-helper path **must leave the compile-time operand stack at
identical height and shape** — the helper consumes the same operands and pushes the same single
result the inlined fast path would. The translator asserts this equivalence per guard site at
compile time; a mismatch is a translator bug, not a runtime condition.

The net effect: **every guard is either (a) a call to the same shared generic helper the
interpreter calls — byte-identical by construction, valid at any point in the frame — or (b) an
entry-only re-run of the whole call on the interpreter — byte-identical because no side effect
preceded it.** There is no partial-frame reconstruction, so there is **no fragile OSR metadata** in
v1.

This is strictly weaker than a production JIT (a mid-loop guard miss resolves to a generic-helper
call rather than cheaply resuming a re-specialized native loop), but it is **tractable and provably
correct**, which is the only acceptable trade for pillar 1.

### 4.2 The deopt metadata that *does* exist in v1

Even entry-only deopt needs a minimal map:

- **Per compiled `FnProto`:** the original `FnProto` (the tier-0 body to re-dispatch to) and the
  argument layout (already available via `proto.params`/`arity`).
- **Per guard site:** which shared generic helper to call on miss (and its operand registers), or a
  flag that this guard re-dispatches the whole call. No operand-stack/locals snapshot is required,
  because v1 never reconstructs a mid-function interpreter state.

This is a deliberately small surface — the explicit goal is to keep the metadata *boring* so the
differential (§5) can actually prove it correct. A future OSR design would add a per-safepoint
register/stack-slot → bytecode-stack/locals map (the real deopt table); that is **out of scope and
a documented non-goal for v1** (§8).

### 4.3 Recursion-depth and the call-depth counter

The interpreter increments `call_depth` exactly once per logical call
(`enter_frame_depth`, `src/vm/run.rs:299`; `MAX_CALL_DEPTH`). The JIT entry path **must perform the
same single increment/decrement per native call** so the `maximum recursion depth exceeded` panic
(SP3 §B) fires byte-identically across tiers. A native call that recurses must thread the same
`Interp.call_depth` `Cell` — the JIT does not get to skip the guard. (Native stack growth via
`stacker`, `src/vm/stack.rs`, applies to the interpreter's async frames; a baseline JIT's native
frames use the OS stack directly within the 512 MB worker stack — the depth *counter*, not the
native stack, is the byte-identical limit.)

## 5. Correctness — the four/five-way differential (THE gate)

This is the non-negotiable core. **JITed code must be byte-identical to the interpreter**, and that
is proven, not asserted.

### 5.1 Extend the differential to a fourth mode

`goal.md` Gate 1 and `CLAUDE.md` already require `tree-walker == specialized-VM == generic-VM` over
the corpus + goldens in both feature configs (`tests/vm_differential.rs`), plus `.aso`-compiled as a
fourth *execution* mode. The JIT adds **JIT-enabled VM** as another mode, so the identity becomes:

> **tree-walker == generic-VM == specialized-VM == JIT-VM** (and `.aso`-compiled, which is just the
> VM over deserialized bytecode and so is covered by whichever VM mode runs it)

over the **entire** example/golden corpus, in **both** feature configs. To force compilation in
tests, a **`JIT_THRESHOLD=0` / "always-JIT" test mode** compiles every eligible `FnProto` on first
call, so the differential actually exercises native code rather than leaving everything cold.

**A JIT-COVERAGE assertion is mandatory, because threshold=0 alone is a false-green trap.** With
whole-function bail (§3.4), any function containing a single unsupported op stays on the interpreter,
so `JIT_THRESHOLD=0` can pass green while **~no native code actually ran** — every proto bailed and
the "JIT differential" silently degenerated into a second interpreter run. The always-JIT mode
therefore **must emit the count and fraction of corpus `FnProto`s actually compiled vs. bailed**
(compiled / (compiled + bailed)) and **fail (or, during early bring-up, loudly warn) if that
fraction is ~0** — i.e. the differential must prove it exercised native code, not merely that it did
not crash. The coverage number is reported alongside the differential result.

**Required differential MODES** (each a distinct configuration the gate runs):
1. **always-JIT** (`JIT_THRESHOLD=0`): every eligible proto compiled — exercises the native fast
   paths. Subject to the coverage assertion above.
2. **always-deopt** ("JIT-stress"): every guard forced to miss — exercises mechanism (a)/(b)
   fallback on every site.
3. **always-JIT + always-deopt COMBINED** (required, not optional): compile everything *and* force
   every guard to miss, so the compiled prologue/calling-convention/entry path is exercised while
   every body op still routes through the shared generic helper. This is the configuration that
   catches a codegen bug that hides whenever *either* knob alone leaves a path cold.

In every mode the identity to assert is the full **tree-walker == generic-VM == specialized-VM ==
JIT-VM** chain. (A companion harness may also force deopt selectively per-op to localize a
divergence, but the COMBINED mode above is the one the gate requires.)

The tree-walker is **never** relaxed to match the JIT. A divergence is **always** a bug in the JIT
codegen or a guard — *fix the codegen, never the assertion* (`goal.md` Gate 1; the same rule that
governs the existing specialized/generic split, `src/vm/run.rs:104`).

### 5.2 Extend the FUZZ differential

The FUZZ campaign item (`goal.md`, FUZZ) stands up a grammar-aware differential fuzzer asserting
the engines agree. **The JIT joins that fuzzer as a mode**: every fuzz-generated program runs on the
JIT-VM (always-JIT mode) and its output/panic must match the tree-walker. The numeric edges NUM
already targets (overflow boundaries, division-by-zero, the 2^53 comparison boundary, wrapping vs
checked) are *exactly* the codegen the JIT is most likely to get subtly wrong, so the JIT must be a
first-class fuzz target — not a manually-tested afterthought. **This is why FUZZ is a precondition's
sibling:** without continuous differential fuzzing, a baseline JIT's correctness cannot be trusted.

### 5.3 Why this gate is the reason for deferral

A diverging JIT is not a performance regression — it is a **wrong-answer bug**, the one thing the
campaign never tolerates. The cost of *building the proof* (the always-JIT differential + JIT fuzz
mode + the deopt-stress harness) is itself substantial, and it only pays off if §1.1's profiling
shows the speed is there to win. Gating on evidence means we don't pay the correctness-infrastructure
cost for a speed win we haven't confirmed exists.

## 6. Per-isolate JIT & the `.aso` (no format change)

### 6.1 Per-isolate code cache

The runtime is `!Send` and share-nothing across worker isolates (`src/worker/`, the workers
foundation spec). The JIT follows the same model: **each isolate has its own JIT compiler, its own
code cache, and its own counters.** There is no shared compiled-code cache across isolates (sharing
native code would require `Send`/synchronization the model rejects, and Cranelift's `JITModule`
owns its code memory). Each isolate **JITs independently** — a `worker fn` that is hot in three
isolates is compiled three times, once per isolate. This is consistent with the workers spec's
"each isolate has its own everything" stance and is the right default; the per-isolate compile cost
is amortized exactly as the interpreter's per-isolate warmup is (the per-isolate pool model, workers
foundation §7; its performance bar is §11.5).

The JIT state lives on the per-isolate `Vm` (the same place `shapes`, `class_methods`,
`user_globals`, and `specialize` live, `src/vm/run.rs:51`), behind the JIT Cargo feature. A worker
isolate that drops its handle drops its JIT code cache with it — deterministic reclamation, like
every other isolate-local resource.

### 6.2 The `.aso` is NOT JIT output

**`ascript build` continues to emit bytecode `.aso` only.** The JIT is a **runtime-only, in-memory**
tier: it compiles bytecode → native code *during execution*, and that native code is never
serialized. Therefore:

- **No `.aso` format change.** `ASO_FORMAT_VERSION` (currently 18, `src/vm/aso.rs:105`) is
  **untouched** by this spec. An `.aso` is loaded, verified (`src/vm/verify.rs`), and run on the
  VM, which then JITs hot functions in memory exactly as it would for a freshly-compiled `Chunk`.
- **No ahead-of-time native compilation here.** AOT-compiling to native (a `--native` *machine-code*
  artifact) is the BIN spec's territory (bundle runtime + `.aso`), and even there the shipped code
  is the runtime + bytecode, not JIT output. A persistent native-code cache on disk is a possible
  far-future optimization, explicitly **out of scope** (§8).

This keeps the JIT strictly additive: it changes *how fast* a `Chunk` runs, never *what* a `Chunk`
is or how it serializes.

## 7. Performance — honest, measured-not-promised

**No speedup number is promised in this spec, by design** — §1.1 precondition 3 (profiling) is what
would establish the headroom, and the JIT only proceeds if that headroom is real. What can be stated
honestly now:

- **Where it can win:** numeric/loop-heavy, monomorphic, allocation-light code — tight loops doing
  integer or float arithmetic and shaped property access, where (post-NUM/VAL) native code keeps
  unboxed values in registers and removes per-instruction dispatch + operand-decode overhead. This
  is the LuaJIT/Sparkplug sweet spot.
- **Where it does NOT win:** allocation-bound code. A JIT cannot remove `Rc`/`Cc` refcount traffic
  or cycle-GC pauses (`src/gc.rs`). On allocation-heavy workloads the bottleneck is the value model
  and the GC, which **VAL** (escape analysis, refcount-churn reduction) addresses — not the JIT.
  Running the JIT on such code would show little or no improvement and is a reason the profiling
  gate matters.
- **The structural ceiling (strengthened — it holds under *both* VAL outcomes):** even with NUM +
  VAL, AScript keeps an `Rc`/`Cc` + cycle-collecting GC value model and gradual runtime contracts.
  **Critically, VAL unboxes only *scalars* — every heap kind (`Str`, `Array`, `Object`, `Map`,
  `Instance`, …) keeps its `Rc`/`Cc` payload in *both* VAL outcomes** (the 8-byte NaN-box still
  stores a tagged `Cc` pointer; the 16-byte niche fallback still boxes the two fat variants — VAL
  §3.3). So **the JIT cannot remove refcount traffic either way** — native code touching a heap
  value still does the same `Cc` clone/drop the interpreter does. **Under the 16-byte niche fallback
  the ceiling is lower still:** the two formerly-inline fat variants become an *extra indirection*
  (a boxed payload) versus the 8-byte NaN-box, so heap-touching native code pays one more pointer
  chase than it would under full NaN-boxing. Either way these cap JIT gains **well below** what a
  statically typed AOT language (Java/C#/Swift/Go) achieves — those have monomorphic layouts, no
  per-call contract checks, and a tuned GC the JIT can assume. AScript's JIT is a **baseline
  interpreter accelerator**, not a path to C-class throughput. Setting that expectation explicitly
  is part of the design (over-promising here would be the failure mode).
- **Compile-cost trade:** a synchronous v1 compile (§2.2) adds latency at the threshold-crossing
  call. Cranelift's fast compile keeps this small, but it is a real cost the profiling/benchmark
  harness must measure (compile time vs. recovered runtime), reported per `bench/` convention. If
  warmup latency shows up, that is the trigger to consider background compilation — not before.

A required deliverable *if and when this is implemented* is a `bench/` report quantifying: speedup
on the numeric/loop corpus, the (non-)effect on allocation-bound code, compile latency, and the
break-even hotness threshold — the same measure-it discipline the workers spec §11.5 follows.

## 8. Scope & rejected alternatives

**In scope (if/when un-deferred):** a method-level baseline JIT using Cranelift; tier-up by
per-`FnProto` call/backedge counting; codegen consuming the existing `adapt.rs`/`ic.rs` feedback;
checked-overflow/wrapping-faithful integer codegen (NUM, via a conditional-branch-to-shared-panic
path, never a CLIF `trap`); **function-entry-only** deopt to the interpreter (the two mechanisms of
§4.1 kept separate); per-isolate code cache; the four/five-way differential + JIT fuzz mode — with
the always-JIT **coverage assertion** and the always-JIT+always-deopt **COMBINED** mode (§5.1); the
`#[cfg(feature="jit")]`-gated, cheaply-homed, benchmarked tier-up counters (§2.1); the benchmark
report. All behind a Cargo feature flag, default-off until the gate (§1.1) is met.

**Out of scope / non-goals:**
- **Mid-function OSR** — deferred even within a future JIT effort; v1 is entry-only (§4.1). The
  fragile deopt-metadata problem is the reason.
- **Async/generator function JIT** — `is_async`/`is_generator` protos stay on the interpreter (they
  suspend across `.await`; the M17 async non-goals forbid continuation capture, `CLAUDE.md`).
- **Background/async compilation** — v1 is synchronous; off-thread compile is a future refinement
  gated on compile-latency evidence (§2.2), complicated by `!Send` + `JITModule` ownership.
- **Persistent on-disk native-code cache / AOT-to-native** — the `.aso` stays bytecode (§6.2);
  native artifacts are BIN's concern and even there are runtime + bytecode, not JIT output.
- **An optimizing tier-2** — out of scope; baseline only.

**Rejected:**
- **LLVM as the code generator.** Compile latency (often 10–100× Cranelift) dominates at JIT
  granularity and pulls in a heavyweight, non-pure-Rust toolchain dependency — wrong for a
  *baseline* tier. Cranelift is the Wasmtime-proven answer for exactly this niche.
- **Mid-function OSR in v1.** The deopt-metadata complexity (reconstruct a full interpreter frame
  at an arbitrary safepoint) is where JIT correctness bugs concentrate (the Self/HotSpot deopt
  literature). Entry-only deopt is provably correct and tractable; OSR is a separate, later design.
- **Implementing before NUM + VAL.** A JIT over boxed `f64` + a fat `Value` + `Rc` has a low
  ceiling — the interpreter's adaptive `f64` path already captures most of the available win, and
  native code still chases pointers (§1, §1.1). Negative ROI.
- **Implementing before profiling proves dispatch is the bottleneck.** Justify by measurement, not
  assumption (`goal.md` pillar 4). If allocation/GC/I/O dominates, the JIT does not help.
- **JIT-compiling to `.aso` / changing the serialization format.** The JIT is runtime-only and
  in-memory; the `.aso` stays bytecode and `ASO_FORMAT_VERSION` is untouched (§6.2). Serializing
  native code would couple the artifact to a target/Cranelift version and break the
  "ship bytecode, run anywhere" property.
- **Relaxing the differential to accommodate a JIT divergence.** Forbidden. A divergence is a JIT
  bug; fix the codegen, never the assertion (`goal.md` Gate 1).

## 9. Grounding (verified sources)

- **Cranelift / Wasmtime** — the Bytecode Alliance code generator designed for JIT/baseline use:
  fast compile, pure-Rust, `cranelift-jit` in-memory code emission. The reference precedent for "a
  baseline native tier without LLVM."
- **V8 tiering (Ignition → Sparkplug → Maglev → TurboFan)** — Sparkplug is V8's *baseline* JIT
  (compile fast, modest codegen, no deopt of its own beyond falling to the interpreter); the
  baseline-tier model and eager-ish tier-up threshold are drawn from it. Maglev/TurboFan are the
  optimizing tiers this spec explicitly does NOT attempt.
- **LuaJIT** — the canonical evidence that a *dynamic* language JIT wins most on numeric/loop-heavy
  monomorphic code and least on allocation-bound code; informs the honest-ceiling framing (§7) and
  the trace/hotness intuition (adapted here to method-level tier-up).
- **HotSpot / interpreter+JIT mixed-mode** — per-method invocation + backedge counters driving
  tier-up; the counter model (§2.1) follows it.
- **Deopt / on-stack replacement literature** — Hölzle, Chambers & Ungar, *"Debugging Optimized
  Code with Dynamic Deoptimization"* (Self, PLDI'92) and the broader Self/HotSpot deopt work — the
  basis for treating mid-function OSR as the subtle, bug-prone part and deferring it (§4.1, §8).
- **PEP 659 (CPython specializing adaptive interpreter)** — the type-feedback model the engine
  already implements in `src/vm/adapt.rs`; the JIT consumes that same feedback rather than inventing
  a new profiler (§3.2).
- **Internal grounding (cited inline):** `src/vm/run.rs` (the dispatch loop / tier-up trigger
  points — `run_loop:581`, frame push / call-counter site `:1253`, `Op::Loop` backedge-counter site
  `:1418`, `enter_frame_depth:299`, `specialize:104`, `eval_binop_adaptive:3743`, and the
  guard-miss → shared `apply_binop` deopt arm `:3798-3803` that mechanism (a) lifts into native
  code); `src/vm/chunk.rs` (`FnProto:335` the unit of compilation, the offset-keyed side-map
  pattern, `arith_cache:607`, `params:358` the shared arg-checker source); `src/vm/adapt.rs`
  (`ArithKind`/`ArithCache` type feedback, `WARMUP_THRESHOLD:44`, the side-map rationale; NUM adds
  `ArithKind::Int` alongside `ArithKind::Number`=float); `src/vm/ic.rs`
  (`InlineCache`/`MethodCache::Mono:147` feedback); `src/vm/opcode.rs` (`operand_width:623`, the
  immutable byte-identical bytecode); `src/vm/aso.rs` (`ASO_FORMAT_VERSION = 18`, untouched). VAL's
  ≤16-byte guarantee and its `Cc::into_raw`/`from_raw` upstream gate: compact-value spec §3.2/§3.3.
