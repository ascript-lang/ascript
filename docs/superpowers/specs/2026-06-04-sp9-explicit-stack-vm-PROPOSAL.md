# SP9 — Explicit-Stack VM (model "2b"): Enabling the Three M17 Async Non-Goals

> **PROPOSAL FOR DISCUSSION — VM-scale, the deepest sub-project.** This is a
> feasibility/design proposal, not a spec and not a plan. It exists to let the
> owner make an informed go/no-go and scope decision. Its central recommendation
> is **do not build all of model 2b** — see §0 and §5.

- **Date:** 2026-06-04
- **Status:** Draft proposal (no implementation, no commit)
- **Branch:** `feat/sp1-engine-parity`
- **Relates to:**
  - `adr/2026-05-30-async-generators.md` — the three deferred non-goals (the "B2" engine)
  - `2026-06-02-bytecode-vm-design.md` — the current VM is **model 2a**
  - SP3 (graceful recursion limit) — the *cheap* answer to non-goal #2

---

## 0. TL;DR / honest recommendation

The ADR defers three architectural non-goals to a hypothetical "explicit-stack VM
with reified continuations" (option B2). This proposal examines what B2 actually
costs **on top of the VM we already have**, and reaches three conclusions:

1. **The VM is already half of "2b".** AScript's VM already has an *explicit frame
   stack* (`Fiber.frames: Vec<CallFrame>`, `src/vm/fiber.rs`). Script→script
   `CALL`/`RETURN` is **already iterative** — it pushes/pops `CallFrame`s and the
   Rust loop stays flat. The native-stack recursion that remains is **not** in the
   bytecode dispatch; it is in two specific places: (a) `#[async_recursion]` on
   `Vm::run` for *native re-entry* (`call_value` for higher-order stdlib callbacks
   and generator `resume`), and (b) tokio owning task scheduling. This materially
   changes the cost picture from what the ADR's "full rewrite" framing implies.

2. **The three non-goals are wildly different in cost, and should be unbundled.**
   - **Non-goal #2 (robust deep recursion)** is *mostly already solved* by the
     explicit frame stack, and the residual native-stack recursion can be removed
     **cheaply** — without any 2b rewrite — via either the `stacker` crate
     (probe-and-grow) or by trampolining the handful of `call_value`/`resume`
     re-entry points. **Recommended.**
   - **Non-goal #3 (deterministic scheduling)** requires replacing tokio with an
     owned single-threaded scheduler **and** taming every non-deterministic I/O
     seam (clock, RNG, network, fs ordering). This is FoundationDB-/Antithesis-class
     work. Valuable for testing, but a large, contained effort. **Optional, later.**
   - **Non-goal #1 (durable/serializable continuations)** is the one the whole
     industry has **abandoned** in favor of event-sourced replay (Temporal,
     Cloudflare Workflows, Restate). Serializing a live continuation that holds
     sockets/files/DB handles is a genuine showstopper, *not* an engineering
     slog. **Recommend NOT pursuing continuation serialization; if durability is
     ever wanted, do replay-based durable execution instead — and that does not
     need a 2b VM at all.**

3. **A full CPS/explicit-continuation rewrite of the VM is comparable to or
   bigger than the entire original bytecode-VM sub-project**, and the differential
   oracle that protected the 2a build would have to be re-paid in full. The payoff
   (durable continuations) is the one item that doesn't actually work. **So the
   recommendation is: ship SP3's graceful limit now, do the cheap "robust
   recursion" upgrade (stacker/trampoline) as a small follow-up, and treat
   deterministic scheduling as an independent opt-in project. Do not build the
   monolithic 2b "reified continuations" VM.**

The rest of this document substantiates each of those claims, with the research
behind them, so the owner can disagree from a position of detail.

---

## 1. What an explicit-stack run loop looks like for AScript

### 1.1 What we already have (the honest starting line)

The ADR (and the 2a spec's framing) describe 2b as a "full rewrite of the
evaluator." That was accurate for the **tree-walker**, but the **bytecode VM has
already done most of the structural work**:

- A `Fiber` (`src/vm/fiber.rs`) is exactly the "explicit stack" data structure
  2b needs: `frames: Vec<CallFrame>` + a single `stack: Vec<Value>` + a `state`.
- A script function `CALL` (`src/vm/run.rs`, `Op::Call` arm) **pushes a
  `CallFrame` and continues the loop**; `RETURN` pops it. There is **no native
  recursion** for script→script calls today. The dispatch is a structured
  `loop { match op { … } }`.
- `await`/`yield` are already opcodes (`Op::Await`, `Op::Yield`). `Op::Yield`
  already "reifies" suspension correctly: it returns `RunOutcome::Yielded(v)`
  **without unwinding frames** — the `Fiber` keeps its live frame stack and `ip`,
  so the next `resume` continues exactly where it left off. **This is already a
  reified, heap-resident continuation** for the generator case (a Suspended
  `Fiber`).

So the gap between 2a and the "explicit-stack run loop" of 2b is **not** "build a
frame stack." It is narrower and specific:

### 1.2 The residual native recursion (the actual 2b delta)

`Vm::run` is `async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control>`
with `#[async_recursion::async_recursion(?Send)]`. The native Rust stack is used —
and can therefore overflow / cannot be reified — only at these points:

1. **`await` driving a future inline.** `Op::Await` does `f.get().await`. The
   awaited future may itself be a spawned task that re-enters `Vm::run`. This is a
   tokio `.await` on the native async stack.
2. **Native → VM re-entry via `call_value`.** Higher-order stdlib functions
   (`array.map`, `filter`, comparators, middleware, `recover`) call back into a
   script closure through `Vm::call_value`, which sets up a fresh `Fiber` and calls
   `self.run(...).await`. *This* is the genuine native recursion: a deep
   `map`-of-`map`-of-`map` or a recursive comparator nests Rust frames.
3. **Generator `resume`.** `GeneratorHandle::resume` (`src/coro.rs`) drives a
   Suspended `Fiber` by `await`ing `Vm::run`; nested generator composition nests
   Rust frames.
4. **Tokio scheduling.** Every spawned `async fn` is a `tokio::task::spawn_local`
   task; interleaving is tokio's, not ours.

A "model 2b" run loop is one where **all four** of these become explicit
heap-managed state instead of native stack / tokio:

```
// Conceptual 2b loop — NOT a recommendation to build it wholesale.
enum Step { Continue, Suspend(Reason), Done(Value) }
enum Reason { Await(FutureId), Yield(Value), CallNative(NativeCont) }

loop {
    let frame = fiber.frames.last_mut()?;            // explicit "current frame"
    match decode(frame) {
        Call(callee, argc)  => fiber.frames.push(new_frame(...)),   // already so
        Return(v)           => { fiber.frames.pop(); push v; }      // already so
        Await(fut)          => return Suspend(Await(fut)),  // <- reified, NOT .await
        Yield(v)            => return Suspend(Yield(v)),    // already so
        CallNative(f)       => return Suspend(CallNative(f.into_continuation())),
        // ... arithmetic / props / etc unchanged ...
    }
}
```

The crux: in 2b the loop **never calls `.await` itself** and **never calls
`call_value` recursively**. Instead, when it needs to wait or re-enter native code
it *returns a suspension reason* to an outer **driver/scheduler** that owns:

- the set of all `Fiber`s,
- a queue of ready/blocked fibers,
- the mapping from `FutureId` → who is blocked on it,
- the bridge to native async I/O (the one place real `.await` happens).

That outer driver replaces both `#[async_recursion]` and tokio's `LocalSet`. The
"reified continuation" is then simply **a `Fiber` (its `frames`+`stack`+`ip`)
plus the scheduler's record of what it is blocked on** — which is *almost* what a
Suspended generator `Fiber` already is, generalized from "blocked on the consumer"
to "blocked on a future / a native call."

This is the design WasmFX formalizes (`suspend` reifies the current execution
context into a first-class continuation value; `resume` re-enters it), and it is
the design CPython 3.11 uses for generators/coroutines (a `_PyInterpreterFrame`
embedded in the generator object, linked into the per-thread frame chain only when
running). AScript's `Fiber` is the analogue of CPython's interpreter-frame chain.

### 1.3 The hard part of the loop rewrite

Two things make this more than a mechanical refactor:

- **Native callbacks become CPS.** Today `array.map(f)` is a Rust `for` loop that
  calls `call_value(f, [elem]).await` per element. In a no-native-recursion 2b, a
  native fn that calls back into script must be re-expressed so it can be
  *suspended and resumed*: it has to yield control to the scheduler, get the
  callback's result later, and continue. That is a **CPS transform of every
  higher-order native function**, or a `corosensei`-style stackful trampoline just
  for the native boundary, or the Lua "throw a yield-error to unwind the C stack
  and a resume-continuation to re-enter" trick (the Lua "fully resumable VM"
  patch). All three are real work and all three are about the *native↔script
  boundary*, which is exactly where the ADR's option B1 (`corosensei`) was aimed.
- **The error model (`Control`/`Flow`) unwind must stay frame-stack-based**, which
  it already is in the 2a VM (the VM "unwinds the explicit Fiber frame stack
  itself" — see the 2a spec's error-handling section). Good: this part is done.

---

## 2. Each non-goal: what it specifically requires on top of the explicit stack

### 2.1 Non-goal #2 — robust unbounded deep recursion (CHEAPEST; mostly done)

**What it requires beyond the explicit stack:** almost nothing structurally,
because the explicit `Fiber.frames` *already* moves script→script recursion off
the native stack. A program that is "just" deeply recursive in script code
(`fn fib(n) = …` style, deep tree walks) already grows `Vec<CallFrame>` (heap),
not the Rust stack. **This non-goal is, for the common case, already satisfied by
the 2a VM** — the 2a spec even claims this ("explicit `frames: Vec<CallFrame>` is
what makes AScript recursion bounded by heap, not the Rust stack … resolves a
CLAUDE.md async non-goal").

The residual overflow risk is the §1.2 native re-entry (deep `map`/`reduce`
nesting, deep generator composition, deep `recover`-wrapped recursion). For *that*
sliver there are three cheap options, none of which need a 2b rewrite:

- **`stacker` crate (probe-and-grow).** `stacker::maybe_grow(red_zone, stack_size,
  || …)` checks remaining native stack and allocates a fresh segment if low. Drop
  one `maybe_grow` guard at the `call_value`/`resume` re-entry points and deep
  native recursion stops overflowing. Smallest possible change; an external,
  widely-used crate; no semantic change. **This is the recommended "robust
  recursion" answer.**
- **Trampoline the re-entry points only.** Convert `call_value`'s recursive
  `run().await` into a loop that pushes the callee `Fiber` onto a *stack of
  fibers* and drives them iteratively, returning the result to the suspended
  native caller. More invasive than `stacker` but no new dependency and no
  `unsafe`.
- **`corosensei` (option B1).** Run each script callback on a switchable stackful
  coroutine stack. Solves it, but adds `unsafe` and a second suspension mechanism
  to reconcile with tokio — the ADR already weighed and rejected this for M17.

**Verdict:** SP3's graceful limit is the right *default* (a clean error beats a
crash); a `stacker::maybe_grow` at the native re-entry points is a cheap,
non-2b upgrade to "robust" for users who genuinely need deep recursion. Neither
needs reified continuations.

### 2.2 Non-goal #3 — deterministic / replayable scheduling (CONTAINED but large)

**What it requires beyond the explicit stack:**

1. **Own scheduler replacing tokio's `spawn_local`/`LocalSet`.** With an explicit
   `Fiber` set, the scheduler is a single-threaded run-queue: pick the next ready
   fiber by a *deterministic* policy (e.g. FIFO with a seeded tiebreak), run it
   until it suspends (`Await`/`Yield`/blocking native I/O), record the suspension,
   pick the next. This is *enabled* by §1.2's "loop returns a suspension reason"
   design — the driver becomes the scheduler. FoundationDB's **Flow** and the
   broader deterministic-simulation-testing (DST) world do exactly this: "a
   deterministic program cannot run on more than one OS thread … so build your own
   concurrency model on a single OS thread." AScript is already single-threaded
   and `!Send`, which is a big head start.
2. **Tame every non-deterministic seam.** This is the real cost, and it is *not*
   in the VM — it is in the stdlib. DST systems abstract **time, RNG, network,
   disk, and thread scheduling** behind injectable interfaces (FoundationDB's
   `INetwork`→`SimNetwork`, `IAsyncFile`→`SimFile`; a seeded `deterministicRandom()`
   replacing all randomness). For AScript that means: a virtual clock
   (`datetime.now`, `task.sleep`), a seeded PRNG (`crypto`/`uuid`/`math.random`),
   and a recording layer over `net`/`fs`/`sql` so the *ordering and results* of
   real I/O are either simulated or recorded-then-replayed.
3. **The fundamental tension:** real I/O is non-deterministic. Two coherent
   answers, both from the research:
   - **Record/replay:** run once for real, log every I/O result + interleaving
     decision; replay feeds the log back so the run is bit-for-bit reproducible
     (Temporal's event-history model; FoundationDB's seeded PRNG drives all
     "random" latencies/crashes).
   - **Full simulation:** never touch real I/O in deterministic mode; a simulated
     network/disk with a seeded PRNG generates plausible latencies and faults
     (Antithesis/FoundationDB). Great for testing, not for production runs.

**Verdict:** genuinely valuable (reproducible test failures, fault injection,
"replay this exact run"), and the *scheduler* part is a natural consequence of the
§1.2 driver. But the *seam-taming* is a large, stdlib-wide effort orthogonal to the
VM. It can be built **incrementally and independently** once the driver exists; it
does **not** require continuation serialization. Treat as its own opt-in project.

### 2.3 Non-goal #1 — durable / serializable continuations (SHOWSTOPPER)

**What it requires beyond the explicit stack:** a serialization format for a paused
`Fiber` — `frames` (each `CallFrame`'s `closure` ref, `ip`, `slot_base`, `cells`),
the operand `stack`, and the scheduler's blocked-on state — that can be written to
disk and faithfully reconstructed in a *fresh process*. The explicit stack makes
the *in-memory* state addressable; the problem is everything the state *points at*:

- **Code identity.** A `CallFrame` holds `Cc<Closure>` → `Rc<FnProto>` → a `Chunk`.
  Serializing an `ip` is only meaningful against a *specific* compiled chunk. The
  `.aso` format already gives us stable, versioned, content-addressable chunks, so
  this part is **tractable**: serialize a chunk-hash + ip, reload the matching
  `.aso`. (This is the one place 2b's groundwork helps durability.)
- **Value graph.** `Value`s in slots/stack/cells must serialize. `Number/Str/Bool/
  Array/Object/Map/Bytes` are fine. Cyclic graphs need the same cycle-aware walk
  the GC already does. Closures-as-values need the chunk-hash trick. Tractable but
  laborious.
- **NATIVE RESOURCES — the showstopper.** A paused workflow that is mid-flight
  almost always holds a `Value::Native` handle: an open TCP socket, an HTTP
  response body being streamed, a SQLite connection/transaction, a child process,
  a TUI terminal, an SSE/WebSocket stream. **None of these can be serialized** —
  they are live kernel/OS state, not data. This is a *universal* finding across the
  durable-execution industry: "database connections and file handlers are
  frequently non-serializable … they represent live resources"; the standard fix
  is to mark them `transient` and **re-establish them on resume** — which means the
  workflow's *logic* must be written to tolerate a connection vanishing and being
  recreated. You cannot transparently freeze a half-read socket and thaw it later.

This is why **the entire durable-execution industry abandoned
serialize-the-continuation** and converged on **event-sourced deterministic
replay** instead:

- **Temporal:** workflow code must be deterministic; the engine persists an
  **event history** (the results of every non-deterministic step). On crash, a new
  worker **re-runs the workflow code from the top**, feeding recorded results back
  so it deterministically reaches the same point — *the continuation is never
  serialized, it is reconstructed by replay.* Side-effecting work (I/O, sockets,
  DB) lives in **activities**, which are *not* replayed — their results are
  recorded.
- **Cloudflare Workflows / Restate / Azure Durable Task Framework:** same pattern.
  Durability comes from a persisted log of step results, not a frozen stack. The
  Restate write-up states it plainly: durability is "the ability to implicitly
  persist state … this ideal breaks down when code holds references to
  non-serializable resources like open file handles, socket connections, or
  database connections."

**Verdict:** serializable continuations are not a "very hard engineering project"
— for a language with a rich native-resource stdlib (which is *AScript's explicit
design goal*: "Go/Deno-class standard library"), they are **architecturally
unachievable in the transparent form the ADR imagines.** The achievable form is
*replay-based durable execution*, and crucially **that does not require a 2b VM at
all** — Temporal implements it for ordinary Java/Go/Python on a normal stack. It
requires (a) a deterministic-execution discipline (which *is* non-goal #3's seam
work) and (b) an event-history persistence layer. If AScript ever wants
durability, that — not continuation serialization — is the path.

---

## 3. Migration + the differential

If, despite §0, some form of 2b is pursued, the migration discipline that
protected the 2a build is mandatory and **roughly doubles** the cost relative to a
naive estimate:

- **Build 2b alongside 2a**, exactly as the VM was built alongside the
  tree-walker. Keep the whole-corpus **byte-identical differential** green
  throughout: `tree-walker == VM-2a == VM-2b` over `examples/`, the full suite,
  and the recorded goldens, in **both** feature configs. The 2a project already
  pays a *three-way* differential (tree-walker == generic-VM == specialized-VM,
  `tests/vm_differential.rs`); 2b makes it **four-way**. Every divergence is a 2b
  scheduler/continuation bug to fix, never an assertion to relax.
- **Determinism is itself a new test oracle.** For non-goal #3, add a
  *same-seed-same-trace* oracle: run a concurrent program twice with the same seed
  and assert bit-identical interleaving + output. This is *additional* to the
  differential (the differential only checks final output equality; determinism
  checks the *schedule*).
- **Replace vs coexist?** The honest answer: 2b would **replace** 2a, not coexist
  — maintaining two production async engines forever is untenable, and the
  specialization/IC layer would have to be re-validated against the new loop. So
  the migration ends with deleting 2a's `#[async_recursion]` run loop and the
  tokio `spawn_local` task model, which is a large, all-at-once cutover (mirroring
  the front-end/VM single-merge cutover the VM spec chose). The §2.1/§2.2/§2.3
  unbundling exists precisely so we **don't** have to take that cutover.

### Honest cost estimate

- **Non-goal #2 via `stacker`:** days. A guard at 2-3 re-entry points + tests.
- **Non-goal #2 via re-entry trampoline:** 1-2 weeks. Contained to `call_value`
  and `resume`.
- **The §1.2 explicit driver + CPS-ifying native callbacks (the 2b loop itself):**
  comparable to the *async/generator slice* of the original VM, which the VM spec
  already flagged as "the risk concentration … may be split into its own
  sub-spec." Call it **a major multi-month sub-project** on its own.
- **Non-goal #3 (scheduler + seam taming + replay log):** another major
  sub-project, stdlib-wide, **on top of** the driver. FoundationDB-class.
- **Non-goal #1 (durable continuations):** the serialization mechanics are weeks;
  the native-resource problem is **not solvable** in the transparent form, so the
  honest cost is "infinite for the stated goal; pivot to replay-based durability,
  which is its own multi-month project layered on #3."

**Total for "all three as a monolithic 2b":** comparable to or **larger than the
entire original bytecode-VM sub-project**, with the largest single item (durable
continuations) being the one that doesn't deliver its promise. The ADR's "by far
the largest cost/risk" assessment of B2 is, if anything, understated for #1.

---

## 4. Scope forks for the owner (decision tree)

```
Do you need deep recursion to not crash?
  ├─ "A clean error at a limit is fine"      → SP3 graceful limit. DONE. (ship now)
  └─ "Must handle genuinely deep recursion"  → SP3 + stacker::maybe_grow at the
                                                native re-entry points. Days. No 2b.

Do you need bit-for-bit reproducible concurrent runs (testing / replay-debug)?
  ├─ "No"   → skip.
  └─ "Yes"  → Build the explicit driver (§1.2) + own scheduler + seam taming
              (virtual clock, seeded RNG, recorded I/O). Major, contained,
              INDEPENDENT project. No continuation serialization needed.

Do you need a paused workflow to survive a process restart (durability)?
  ├─ "No"   → skip.
  └─ "Yes"  → DO NOT serialize continuations (native resources make it
              impossible). Build REPLAY-based durable execution (Temporal model):
              deterministic workflow discipline (reuses the #3 seam work) +
              event-history persistence + activity/workflow split. Major project;
              does NOT require a 2b VM.
```

**Key forks, stated bluntly:**

- **Fork A — "robust recursion" is decoupled and cheap.** Do not let it justify
  2b. `stacker` or a small trampoline gets it. *Recommend taking this.*
- **Fork B — durable continuations is a mirage; replay is the real thing.** The
  ADR bundles "durable continuations" with "needs a 2b VM." Both halves are wrong:
  the achievable durability is replay-based, and replay-based durability does not
  need 2b. *Recommend NOT building continuation serialization at all.*
- **Fork C — deterministic scheduling is the only non-goal that genuinely wants
  the explicit driver,** and even it wants the driver + a large stdlib seam effort
  more than it wants "reified continuations." If any single non-goal justifies the
  driver, it is #3 — but its value is mostly *testing/debugging*, so weigh it
  against that, not against production need. *Recommend treating as an independent
  opt-in project, deferred until there's concrete demand.*
- **Fork D — coexist vs replace.** 2b cannot coexist with 2a long-term. Any
  decision to build the driver is implicitly a decision to eventually delete the
  tokio-based 2a engine and re-validate the entire specialization layer against the
  new loop. That is the true blast radius. *Recommend not opening this unless #3
  has hard demand.*

---

## 5. Recommendation

**Do not build the monolithic model-2b "explicit-stack VM with reified
continuations."** Specifically:

1. **Ship SP3's graceful recursion limit** as the default behavior. A clean,
   source-pointed Tier-2 error at a configurable depth is the correct product
   behavior and is consistent with AScript's "no hidden control flow" ethos.
2. **Add a cheap "robust recursion" upgrade** (a `stacker::maybe_grow` guard, or a
   small trampoline, at the `call_value`/generator-`resume` re-entry points). This
   is the *entirety* of non-goal #2's residual, and it is days—not months—of work
   with no `unsafe` and no engine rewrite. Update the ADR to record that the
   explicit `Fiber` frame stack already satisfied the common case and this guard
   closes the native-re-entry sliver.
3. **Reclassify non-goal #1 (durable continuations) from "deferred, needs 2b" to
   "won't-do as continuation serialization; achievable only as replay-based
   durable execution, which is a separate non-VM project."** This is the single
   most important correction: it removes the main thing that made 2b look
   mandatory. Cite the industry convergence (Temporal/Cloudflare/Restate) and the
   native-resource non-serializability finding.
4. **Keep non-goal #3 (deterministic scheduling) as a genuine, deferred,
   *independent* opt-in project** — valuable for reproducible testing/replay-debug
   — and note that *if* it is ever built, it is what naturally introduces the
   §1.2 explicit driver/scheduler, and it (not #1) is the real reason one might
   eventually want a 2b-shaped loop. Even then, build the driver incrementally
   behind the existing four-way differential; do not big-bang it.

In short: **the deepest sub-project is mostly unnecessary.** The frame stack we
already have + a stack-grow guard delivers the one non-goal users will actually
hit (#2). The headline non-goal (#1) is unachievable in the imagined form and
achievable in a different, non-2b form. Only #3 wants the explicit driver, and its
value is specialized enough to defer until demand is concrete. The honest move is
to **rewrite the ADR's deferral section** with these distinctions rather than to
build B2.

---

## 6. Open design questions (prioritized)

1. **(Decision) Do we accept Fork B?** I.e., do we formally record that
   continuation serialization is won't-do and that durability, if ever pursued,
   is replay-based? Everything else hinges on this.
2. **(Recursion) `stacker` vs trampoline for the native-re-entry sliver?**
   `stacker` is smaller and proven but adds a dep and relies on a red-zone
   heuristic; a trampoline is dependency-free but touches `call_value`/`resume`.
   Which fits the "tiny core" ethos better?
3. **(Recursion) What is the right *default* SP3 limit, and should the
   stacker/trampoline "robust" mode be opt-in (a flag / `Vm` mode) or always-on?**
   Always-on robust recursion changes the failure mode from "clean error" to "use
   memory until OOM," which may be *less* desirable as a default.
4. **(Scheduling) If #3 is ever built, record/replay vs full simulation vs both?**
   Production durability wants record/replay; fault-injection testing wants
   simulation. Which seams (clock, RNG, net, fs, sql) are in scope, and in what
   order?
5. **(Scheduling) Determinism boundary:** is determinism promised only in an
   explicit "deterministic mode," or is the goal that *all* runs are
   deterministic? The former is FoundationDB-style and far cheaper; the latter
   constrains the whole stdlib.
6. **(Architecture) If the explicit driver is ever built, how do native async
   builtins (reqwest/sqlite/tokio I/O) integrate?** They are the one place real
   `.await` must happen; the driver must expose a "park this fiber on this real
   future" primitive without re-introducing native recursion. (This is the WasmFX
   "the host owns the one real stack switch" question and the Lua "yield across the
   C boundary" problem — cite as prior art if pursued.)
7. **(Differential) Can the four-way differential (tree-walker == 2a-generic ==
   2a-specialized == 2b) actually stay green during a driver migration,** given
   that 2b changes *scheduling order* — which the current differential does not
   pin? We may need a determinism oracle *before* the driver, not after.
8. **(Cutover) If 2b ever replaces 2a, how is the specialization/IC layer
   re-validated against the new loop** without re-running the entire V11
   specialization effort? Is the IC layer loop-shape-independent enough to port?

---

## 7. References (research behind this proposal)

- **CPython 3.11 frames / zero-cost exceptions / generators-as-embedded-frames** —
  the canonical "reify the frame, link it into the stack only when running" design
  AScript's `Fiber` mirrors:
  - <https://github.com/python/cpython/blob/main/InternalDocs/frames.md>
  - <https://docs.python.org/3/whatsnew/3.11.html>
  - <https://github.com/python/cpython/issues/84403> ("zero cost" exceptions)
- **Lua resumable VM / yield across the C boundary** — the native↔script
  suspension problem (the crux of §1.3) and the "throw a yield-error to unwind the
  C stack" technique:
  - <http://lua-users.org/wiki/ResumableVmPatch>
  - <https://lua-l.lua.narkive.com/PMhiafpD/patch-fully-resumable-vm-yield-across-pcall-callback-meta-iter>
- **WasmFX / WebAssembly stack-switching (effect handlers, reified continuations)**
  — the formal model of `suspend` reifying the current context into a first-class
  continuation and `resume` re-entering it:
  - <http://wasmfx.dev/>
  - <https://github.com/WebAssembly/stack-switching/blob/main/proposals/stack-switching/Explainer.md>
- **Temporal durable execution (event-history replay; deterministic workflow vs
  non-deterministic activity split)** — why the industry does NOT serialize
  continuations:
  - <https://docs.temporal.io/encyclopedia/event-history>
  - <https://docs.temporal.io/workflow-execution>
- **Restate / Cloudflare Workflows — durable execution & the non-serializable
  resource problem:**
  - <https://www.restate.dev/what-is-durable-execution>
  - <https://developers.cloudflare.com/workflows/get-started/guide/>
- **FoundationDB Flow / deterministic simulation testing (own single-thread
  scheduler; seeded PRNG; `INetwork`/`IAsyncFile` seams)** — the model for
  non-goal #3:
  - <https://apple.github.io/foundationdb/testing.html>
  - <https://pierrezemb.fr/posts/diving-into-foundationdb-simulation/>
  - <https://antithesis.com/docs/resources/deterministic_simulation_testing/>
- **Rust-specific constraints & cheap recursion options:**
  - `corosensei` (stackful coroutines in Rust, `unsafe` stack switch) —
    <https://github.com/Amanieu/corosensei>
  - `stacker` (probe-and-grow native stack) — the recommended cheap path for #2 —
    <https://docs.rs/stacker>
  - `tramp` (trampolining for constant-stack recursion in Rust) —
    <https://docs.rs/tramp>
  - StackSafe — taming recursion in Rust without stack overflow (survey of the
    techniques) — <https://fast.github.io/blog/stacksafe-taming-recursion-in-rust-without-stack-overflow/>
