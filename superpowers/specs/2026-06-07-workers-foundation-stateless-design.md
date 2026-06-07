# AScript Workers — Spec A: Foundation & Stateless Workers

- **Status:** Draft for review
- **Date:** 2026-06-07
- **Depends on:** nothing (this is the foundation)
- **Depended on by:** Spec B (`2026-06-07-workers-stateful-actors-streaming-design.md`)
- **Engines:** both (tree-walker oracle == VM, byte-identical)

---

## 1. Summary & motivation

Today an AScript program runs on a single OS thread (one `current_thread` tokio runtime +
`LocalSet`, one `Interp`, one `Vm`, one thread-local GC heap). `async`/`await` gives full
**I/O concurrency**, but a CPU-bound program can never use more than one core. The runtime is
`!Send` by deep architectural choice (`Rc`/`RefCell`/`Cc`, thread-local cycle GC) — see
`CLAUDE.md` §"The interpreter" and the main design spec §7.

This spec adds **multi-core parallelism without making the runtime `Send`**, by running
additional, complete copies of the runtime on other threads that **share no memory** and
communicate only through **copied (serialized) messages**. This is the shared-nothing isolate
model used by every mainstream dynamic runtime that reached multicore (CPython per-interpreter
GIL/PEP 684, Ruby Ractors, Node `worker_threads`/Web Workers) and validated for Rust by the
thread-per-core runtimes (glommio, monoio). The alternative — an `Arc`/locked/concurrent-GC
`Send` rewrite — was rejected (see §12) because it pays a measured 5–32% single-threaded tax
(Swift BRC paper PACT'18; CPython PEP 703) and is structurally incompatible with the
determinism/replayable-scheduling design value (SP9 durable workflows).

**Spec A covers the foundation plus the stateless, pooled lifecycle** (`worker fn` and
`static worker fn` — request/response). **Spec B** builds the dedicated-isolate lifecycle
(`worker class` actors, `worker fn*` streaming generators) on top of this foundation.

### Supersession note

The main design spec's non-goal *"No multithreading in user code (single-threaded event
loop)"* is **refined, not violated**: there is still no *shared-memory* multithreading and no
data races in user code. Workers are shared-nothing isolates; the single-threaded execution
model holds *within* each isolate. Parallelism is achieved by isolation, not by shared mutable
state.

## 2. The model: one keyword, two layers

A `worker` is a **shared-nothing isolate** you talk to only through copied messages. This
introduces a two-layer communication model that the rest of the stdlib already lives inside:

- **Intra-isolate (within one heap):** the `events` bus, `std/sync` channels/semaphores, async
  tasks. Cheap, shares references, **callbacks/closures work**. Unchanged by this spec.
- **Inter-isolate (across heaps):** `worker fn` (this spec) and the Spec B forms.
  **Serialized messages only** — no shared references, no callbacks.

The boundary between the layers is exactly the **sendability line** (§5).

There is exactly **one keyword** (`worker`) and, across both specs, two lifecycles. This spec
implements the **stateless/pooled** lifecycle:

| You write | It is | Lifecycle | Returns |
|---|---|---|---|
| `worker fn f()` | stateless task | pooled, ephemeral | `future<T>` |
| `static worker fn` (on a class) | stateless task, namespaced | pooled, ephemeral | `future<T>` |

## 3. Surface syntax & semantics

`worker` is a **contextual keyword** (not reserved; like `step`/`as`) that prefixes a function
declaration, sitting exactly where `async` does. In the AST it is a **flag on the function
declaration** (`is_worker`), parallel to the existing `is_async` — *not* a new `ExprKind`,
`Stmt`, or `Pattern` variant.

```
worker fn render(scene) { ...cpu heavy... }
let img = await render(scene)               // call site identical to any async call

class Img { static worker fn encode(px) { ... } }
let png = await Img.encode(px)
```

Semantics:

- **Calling a `worker fn` returns `future<T>`** and dispatches the body to the pool. You
  `await` it. Identical ergonomics to `async fn`; composes with `task.gather`/`race`/`timeout`
  and structured concurrency for free.
- The body **may use `await` internally** — the isolate has its own runtime. `worker` therefore
  subsumes async-at-call-site; **`async worker fn` is not a thing** (rejected as redundant; a
  `worker fn` body may already await).
- A `worker fn*` (generator) and instance `worker fn` methods are **out of scope for Spec A**
  (Spec B and rejected-alternatives respectively).

## 4. Capture & purity rules (compile-time checked)

A `worker fn` runs in a different heap, so it cannot reach the caller's heap by reference. New
checker rule (`worker-capture`, default **Error** — this is a correctness gate, not a lint):

- **Allowed:** parameters; other top-level functions; immutable (`const`) bindings — consts are
  **copied** (structured-clone'd, §5) into the isolate at dispatch.
- **Compile error:** referencing a mutable outer `let`; **mutating** any top-level mutable
  global from within a `worker fn` body.

This keeps stateless workers **referentially stable** (result depends only on args + immutable
consts + top-level fns), which is what makes pool-isolate reuse safe by construction.

## 5. The boundary: a structured-clone value serializer

The single largest work item. Only **serialized bytes** cross threads — never a `Value`, never
the `Interp`. The serializer is the airlock that keeps the runtime `!Send`.

Semantics are **adopted wholesale from the WHATWG structured-clone algorithm** (a rigorously
specified, battle-tested answer for "faithfully copy a value across an isolate boundary"), not
invented:

- **Sendable kinds:** `Nil`, `Bool`, `Number`, `Decimal`, `Str`, `Bytes`, `Array`, `Object`,
  `Map`, `Set`, `Enum`, `Regex` (re-compiled from source on the far side), and **class
  `Instance`s** — reconstructed on the far side by **class identity + cloned fields** (the same
  shape machinery `validate_into` uses; the far isolate must have the class definition, which is
  guaranteed by code-shipping, §6).
- **Cycles are handled** via a visited-reference table (serialize each container once, refer by
  id) — exactly how structuredClone avoids infinite recursion. This is mandatory, not optional.
- **Map key canonicalization** (−0.0→+0.0, NaN unified) is preserved across the boundary
  (reuse `MapKey`).
- **Not sendable → a recoverable Tier-2 panic with the field path** (our analog of
  `DataCloneError`): `Function`/`Builtin`/`Closure`, `Native` resource handles, `Future`,
  `Generator`. Message form: `value of kind <Kind> cannot be sent to a worker at <path>` (e.g.
  `arg[1].cb`). When the offending value is an `events` emitter or a `std/sync` channel
  (Native), the message appends the fix hint: *"event emitters / channels are isolate-local;
  communicate across workers via worker results (Spec A) or actor/generator messages (Spec B)."*

The serializer is implemented at the **`Value` layer** (engine-agnostic), so both engines use
it unchanged.

## 6. Code shipping (dependency closure + bytecode)

On **first dispatch** of a given `worker fn` to an isolate, the runtime ships the function's
**compiled bytecode plus its transitive top-level dependency closure** (the top-level functions
and consts it references, recursively) to the isolate, which **caches** it keyed by function
identity. Subsequent dispatches of the same function to that isolate pay only arg/result
serialization.

- Reuses the `.aso` serialization machinery (`src/vm/aso.rs`) for the bytecode payload.
- The **dependency-closure computation** (function → transitive top-level deps) is the second
  significant work item. It walks the compiled `Chunk`'s constant/global references.
- For the tree-walker oracle, the equivalent is shipping the AST closure; the abstraction is
  "the code slice needed to run this function," materialized per engine.

## 7. The pool (stateless lifecycle)

- **Lazy + demand-grown.** Two gates:
  1. *Compile-time:* if the program contains **zero** `worker fn` declarations, **no pool
     machinery exists** — zero overhead for normal scripts.
  2. *Runtime:* even when worker fns exist, the pool is created on the **first actual call**.
- **Demand-grown to `num_cpus`.** Isolates are spawned on demand up to the cap, not all at
  once. Cap overridable via the **`ASCRIPT_WORKERS`** env var (matching the `ASCRIPT_LOG` /
  `ASCRIPT_CACHE` convention).
- **FIFO work queue with backpressure** for oversubscription: calls beyond live-isolate
  capacity get a pending `future<T>` and are dispatched as isolates free up. Parallelism is
  bounded to pool size (this *is* the scheduler — it prevents a `scenes.map(render)` over 10k
  items from spawning 10k threads).
- **Nested worker calls run inline.** A `worker fn` called from *inside* a pool isolate runs
  **inline (locally) in that isolate**, not re-dispatched. This is deadlock-free (avoids the
  bounded-pool self-starvation), needs zero static analysis or gang scheduling, and matches
  OpenMP's proven default for nested parallelism. Documented as an explicit semantic.

**Cost model (for docs):** birthing an isolate is ~0.5–2 ms (thread + `Interp::new` +
`global_env` builtin install) and reserves ~512 MB *virtual* (not resident) stack
(`WORKER_STACK_SIZE`). The pool amortizes this to ~0 per call after warmup; the per-call cost
is then just arg/result serialization. Therefore: **parallelize coarse work, not tight inner
loops.**

## 8. Cancellation & error propagation

- **Cancel-on-drop across the boundary:** dropping a worker-call `future` sends an abort signal
  across the channel so the isolate stops the in-flight job and is reclaimed by the pool —
  extending the existing `SharedFuture`/`AbortHandle` cancel-on-drop semantics.
- **Errors:** a `[value, err]` Result returned from a worker crosses as **ordinary data**
  (`?`/`!` unchanged). An **uncaught Tier-2 panic** inside the worker is re-raised as a
  **recoverable** panic on the caller (carrying the worker's message + worker-side span
  context), catchable by `recover`.

## 9. Determinism & the differential oracle

- Each isolate replays deterministically on its own — SP9 determinism context is per-`Interp`
  and unchanged.
- Cross-isolate dispatch is **recorded nondeterminism of the same class the async model already
  has**: `task.gather` preserves order, `race`/completion-order do not. The oracle already
  handles async programs; workers add no fundamentally new observable nondeterminism beyond
  parallel timing.
- **`tree-walker == specialized-VM == generic-VM` byte-identical holds**, because the feature
  operates at the `Value`/`Interp` layer both engines share. Differential tests use
  worker programs whose *output* is order-deterministic (gather + ordered consume).

## 10. Implementation surface & cross-cutting subsystems

Every subsystem a new surface form touches, per the `CLAUDE.md` "Touching syntax" checklist
**and** the checker/LSP/editor toolchain. **Each item is a required deliverable** — the feature
is not done until all are updated and green in both feature configs.

**Front-ends (two parsers):**
- `worker` contextual keyword (not reserved) in BOTH the legacy `src/parser.rs` (the oracle
  front-end) AND the CST parser (`src/cst/`).
- `is_worker` flag on the function-decl AST node (parallel to `is_async`); resolver classifies
  the decl as it does any top-level fn.

**Tree-sitter grammar (`tree-sitter-ascript/`):**
- Add the `worker` modifier to `grammar.js`; regen `parser.c` with `tree-sitter generate
  --abi 14`.
- Update the **queries**: `queries/highlights.scm` (tag `worker` as a keyword/modifier so every
  tree-sitter editor colors it); review `locals.scm`/`tags.scm` for impact.
- **Publish the grammar (mandatory whenever `tree-sitter-ascript/**` changes** — CLAUDE.md /
  CONTRIBUTING): run `./scripts/sync-grammar.sh` (subtree-split + push to the
  `ascript-lang/tree-sitter-ascript` mirror; prints the new SHA), then bump that SHA in
  **`editors/zed/extension.toml`** (`commit`) and **`editors/nvim/lua/ascript/treesitter.lua`**
  (`revision`). CI `mirror-grammar.yml` auto-mirrors, but the editor-pin bump is manual.

**Editor integrations (`editors/`)** — three keyword/highlight surfaces exist beyond the
canonical grammar queries; all need `worker`:
- **VS Code TextMate grammar** `editors/vscode/syntaxes/ascript.tmLanguage.json` — add `worker`
  to the keyword/storage-modifier pattern (TextMate highlighting runs independently of the LSP,
  so semantic tokens alone are insufficient). `editors/vscode/language-configuration.json` is
  brackets/comments only — expected to need no change.
- **Zed** `editors/zed/languages/ascript/highlights.scm` and **Neovim**
  `editors/nvim/queries/ascript/highlights.scm` each bundle their own `highlights.scm` copy; add
  the `worker` keyword highlight to both (in addition to the canonical
  `tree-sitter-ascript/queries/highlights.scm`).
- Extend `editors/nvim/tests/treesitter_spec.lua` if it asserts on keyword tokens.

**Formatter (`src/fmt.rs` + `ast.rs` `Display`):**
- Render the `worker` modifier in canonical position. Canonical declaration order:
  `static? worker? fn name(...)`. Formatting is idempotent; add formatter goldens. `ast.rs`
  `Display` renders `worker` identically to the formatter.

**Checker & types (`src/check/`):**
- New rule **`worker-capture`** (default **Error** — correctness gate, not a lint): mutable-`let`
  capture or top-level-global mutation inside a `worker fn` body (§4). Add to `rules::ALL`.
- **Type inference (SP10, `src/check/infer/`):** a `worker fn` call site synthesizes `future<T>`
  exactly as an `async fn` call does, so all downstream `await` / `possibly-nil` /
  `type-mismatch` reasoning is unchanged. **Invariant:** `examples/**` still emits **zero**
  `type-*` diagnostics in both feature configs.
- **Call-arity (`src/check/std_arity.rs`):** add entries for any stdlib fns this spec exposes
  to script (none in the Spec A core; the Spec B `pipe` helper registers here).

**LSP (`src/lsp/`):**
- **Semantic tokens:** `worker` emitted as a keyword/modifier token.
- **Hover (`infer::hover_type_at`):** hovering a `worker fn` shows it is a worker and that calls
  yield `future<T>`.
- **Diagnostics:** `worker-capture` flows through the existing `check::analyze` → LSP path.
- **Navigation (`src/lsp/workspace.rs`):** go-to-def / find-references / rename / workspace &
  document symbols recognize `worker fn` (ordinary named fns + a flag → existing index covers
  them once the parser sets the flag; add LSP tests to confirm).
- **Completion:** offer `worker` as a declaration modifier wherever `async`/`fn` are offered.

**REPL (`src/repl`):** `worker fn` bodies use braces → existing delimiter-depth `is_incomplete`
buffering handles multi-line entry unchanged; cross-line persistence works via the session
`Vm`/`Interp` like any top-level decl. Add a regression test.

**Runtime / new modules:** a worker module (`src/worker/` or `src/stdlib/worker.rs`) — isolate
spawn + per-thread runtime bootstrap (generalize `src/lib.rs` worker-thread setup), the pool,
FIFO queue, code-slice cache, channel dispatch; a `Value` structured-clone serializer
(`src/worker/serialize.rs`); the sendability check. Cross-thread transport uses `Send` byte
channels (tokio mpsc/oneshot) — the awaiting *futures* stay on the caller thread; only bytes
cross.

**`.aso`:** add `is_worker` to serialized function/proto layout → **bump `ASO_FORMAT_VERSION`**
(`src/vm/aso.rs`) and update `src/vm/verify.rs`.

**Docs:** a workers page under `docs/content/` (language guide) covering the model,
`worker fn`/`static worker fn`, the cost model, capture rules, and sendability — **and add its
slug to the `NAV` array in `docs/assets/app.js`** (sidebar + cmd-K search derive from `NAV`; no
entry ⇒ unreachable). Update `README.md` if its feature/stdlib table is affected. **A
comprehensive final documentation consistency & staleness sweep** (README + `docs/` + `CLAUDE.md`
+ the main design-spec non-goal + `roadmap.md`, plus a whole-doc-set sanity check) is specified
as a required deliverable in **Spec B §8.2** and runs after both specs land.

**Tests:** `frontend_conformance.rs`, `treesitter_conformance.rs`, `vm_differential.rs` (both
configs), `check.rs` (the new rule), `lsp.rs` (tokens/hover/nav), plus the unit/integration
tests in §11.

**Unchanged:** `Value` (`Rc`/`Cc`), `Interp` internals, the GC, all existing stdlib, the
single-threaded hot path.

## 11. Testing, example corpus & performance

### 11.1 Unit & checker tests
- **Serializer** round-trip for every sendable kind incl. cycles, nested classes, Map/Set key
  canonicalization; sendability rejection (function, native, future) with correct field path in
  the message.
- **`worker-capture`** checker: const capture OK; mutable-`let` capture and top-level mutation
  error.

### 11.2 Integration (`tests/`, spawning the built binary)
Parallel `map`+`gather` correctness; oversubscription (more calls than pool size) completes via
the queue; nested worker call runs inline (no deadlock); cancel-on-drop reclaims the isolate;
worker panic → recoverable on caller; `[value, err]` crosses as data; lazy-pool proof (a
no-worker program creates **no** pool / no worker threads).

### 11.3 All-modes execution (REQUIRED)
Every worker example `.as` (§11.4) must produce **identical, order-deterministic output** when
run in **all four execution modes**, wired into the existing differential harness
(`vm_differential.rs` already runs the whole `examples/` corpus byte-for-byte; extend it to add
the generic-VM and `.aso` passes for worker programs):
1. **Tree-walker** oracle (`--tree-walker` / `ASCRIPT_ENGINE=tree-walker`).
2. **Specialized VM** (default).
3. **Generic VM** (`--no-specialize` / `Vm::new_generic`).
4. **`.aso`-compiled** (`ascript build` → `ascript run file.aso`) — proves worker bytecode +
   the shipped code-slice survive serialization.

Examples are written to be order-deterministic (use `task.gather`, which preserves order, and
ordered consumption) so byte-identical comparison is meaningful despite parallel timing. This is
the core guarantee that workers behave the same on every engine.

### 11.4 Example corpus (`examples/` — runnable, doubles as docs & all-modes tests)
Create, all verified with `target/release/ascript run <file>` and exercised by the conformance +
differential suites:
- `examples/workers_parallel_map.as` — parallel `map`+`gather` over data (the canonical
  CPU-bound fan-out, e.g. hashing/transform N blocks).
- `examples/workers_static_method.as` — `static worker fn` on a class.
- `examples/workers_nested_inline.as` — a worker fn calling a worker fn (shows inline nesting,
  no deadlock).
- `examples/workers_errors.as` — worker panic caught with `recover`; a sendability violation
  producing the path error.
- `examples/advanced/workers_sample_sort.as` — **parallel sort done right**: chunk → parallel
  `sort` per chunk → k-way merge (the "parallelize data, not recursion" pattern).
- `examples/advanced/workers_monte_carlo.as` — embarrassingly-parallel π estimate across cores.
- `examples/advanced/workers_parse_files.as` — parse N files in parallel, gather results,
  fully error-handled.

### 11.5 Performance measurement (REQUIRED, reported)
A benchmark harness under `bench/` (reusing `src/stdlib/bench.rs`) that **quantifies the actual
benefit** and writes a markdown report (sibling to `bench/PROFILING_RESULTS.md`):
- **Speedup vs cores:** run a CPU-bound workload (Monte Carlo / sample-sort / block hashing)
  sequentially and via workers at `ASCRIPT_WORKERS` = 1, 2, 4, 8, …; report wall-clock, **speedup
  factor**, and **parallel efficiency** (speedup ÷ cores).
- **Serialization overhead vs payload size:** per-call round-trip cost as the arg/result size
  grows — substantiates the "parallelize coarse work, not tight loops" guidance and identifies
  the payload size at which workers start to pay off.
- **Pool warmup cost:** first-call (cold) vs steady-state (warm) latency.
- **Engine note:** headline numbers are on the **VM** (the production engine); tree-walker
  numbers are informational.
- **Expectation (documented, not a hard CI gate — CI core counts vary):** a CPU-bound workload
  should show clear super-1× speedup scaling with cores (e.g. ≳3× on 4 cores for coarse work);
  the report states the measured figures and the break-even payload size.

## 12. Scope & rejected alternatives

**In scope (Spec A):** `worker fn`, `static worker fn`; the structured-clone serializer +
sendability; dependency-closure + bytecode shipping; the pool (lazy, demand-grown, FIFO,
inline-nesting); cancel + error propagation; the `worker` keyword across both front-ends;
`.aso` bump; determinism-at-boundary for request/response; differential/conformance tests;
docs.

**Out of scope (Spec A):** everything stateful → Spec B.

**Rejected:**
- **Full `Send` rewrite** (`Rc`→`Arc`, concurrent/atomic GC, locked containers, multi-thread
  tokio). Measured 5–32% single-threaded tax (Swift BRC PACT'18; CPython PEP 703), statically
  unavoidable in a modular runtime, and forfeits replayable scheduling. Isolation gives
  multicore without any of that.
- **Static thread-reservation / gang-scheduling for nested parallelism.** Nesting depth is
  undecidable in general (recursion, first-class fns, dynamic dispatch); gang scheduling adds
  idle-reserved-core, starvation, and fragmentation footguns. Inline nesting (OpenMP default) is
  correct and free.
- **Fresh-isolate-per-call as the default.** ~1 ms + 512 MB-virtual per call; N simultaneous
  calls = N threads. The pool's bounded size is the throttle.
- **`@decorator` syntax.** A whole new grammar subsystem to get one capability; the `worker`
  keyword mirrors `async` at a fraction of the cost. A config-carrying decorator may be added
  *on top of* the keyword later if per-function tuning is ever needed — purely additive, not in
  scope now.

## 13. Grounding (verified sources)

- Isolation-for-multicore precedent & risk basis: CPython PEP 684; Ruby Ractor (#17100).
- `Send`/atomic-RC tax: Swift biased-reference-counting paper (PACT'18); CPython PEP 703.
- `!Send`-per-core viability: Rust async WG executor-styles doc; glommio; monoio.
- Nested parallelism: OpenMP `OMP_MAX_ACTIVE_LEVELS` (serialize-inner default); Cilk-5 (PLDI'98)
  work-stealing; Java `ForkJoinPool` (helping + compensation).
- Boundary copy semantics: WHATWG structured-clone algorithm (MDN; HTML spec §2.7).
