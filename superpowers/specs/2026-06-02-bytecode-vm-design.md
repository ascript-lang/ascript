# Bytecode Compiler + VM + GC — Design Spec (Runtime, sub-project #2)

**Date:** 2026-06-02
**Status:** Approved (brainstorming complete; pending implementation plan)
**Depends on:** the CST front-end + name resolver (sub-project #1,
`2026-06-02-cst-frontend-migration-design.md`). The compiler consumes the typed AST + resolver output.

> **Scope note:** the cycle-collecting GC is **folded into this sub-project** (it shares the value
> model, closure cells, and Fiber structures the VM rewrites). It is the **final phase**: the VM is
> built on `Rc` to behavioral parity first, then a single `Rc → Cc` migration + collector lands. GC is
> therefore no longer a separate sub-project.

## Problem & context

The runtime pivot (recorded in the front-end spec) replaces AScript's async **tree-walking interpreter**
(`src/interp.rs`, ~6,500 lines) with a **bytecode compiler + virtual machine**. A tree-walker re-walks
the AST on every evaluation; a bytecode VM precomputes structure once and executes a compact instruction
stream, which is both faster and the foundation for deep recursion and future optimization. The CST is
the *tooling* source-of-truth; **the runtime executes bytecode**, not the CST.

This is the single largest, highest-risk sub-project. The async/generator integration is the risk
concentration and may be split into its own sub-spec during planning.

## Goals

- A **stack-based bytecode VM** that runs all of AScript with **no observable behavior change** vs the
  tree-walker, verified by differential testing.
- A **measurable speed-up** (target ≥2× on compute-bound code; **no regression on any** benchmark).
- **Deep recursion** bounded by heap, not the Rust call stack (explicit frame stack).
- **In-spec performance layer**: object/instance **shapes** + **polymorphic inline caches** +
  **PEP 659-style adaptive specialization** — not deferred (these are core dynamic-language benefits).
- **On-disk bytecode** (`.aso`): `ascript build`, plus `import` resolving compiled modules.
- Preserve every M17 async feature (await, generators, structured concurrency, cancel-on-drop,
  `http.serve`) and AScript's error model (`?`/`!`/`recover`, Tier-1/Tier-2) and diagnostics quality.
- **Cycle-collecting GC** (`gcmodule` `Cc` + Bacon–Rajan trial deletion) so reference cycles in
  `Rc`-backed mutable data + closures are reclaimed — **without** breaking deterministic native-resource
  cleanup. Closes the unbounded-growth risk for long-running programs (`http.serve`, daemons).

## Non-Goals

- **Model 2b** (serializable continuations / deterministic scheduling) — VM is model **2a** (async
  dispatch loop borrowing tokio's suspension). Consistent with CLAUDE.md §7 async non-goals.
- **Register-based ISA** — stack-based by decision (research: register's edge shrinks for dynamic
  languages where boxing/`Rc` dominate dispatch; modern wins come from specialization, not ISA).
- **Threaded dispatch** (computed-goto / tail-call interpreter) — blocked by stable Rust (no
  labels-as-values; `become` unstable). Documented constraint with a revisit trigger, not a silent drop.
- **Native `.so`/`.dll` plugin import (FFI)** — a separate future spec; unrelated to bytecode.
- **Multi-threaded execution** (Level 1 native offload / Level 2 isolates) — separate future work; the
  VM is designed isolate-friendly (no new globals/thread-locals) so it stays a clean additive step.
- **CacheIR-style structured IC IR** — noted as the north star (esp. for a future JIT); v1 uses direct
  inline caches.

## Architecture

```
typed AST (from CST)  +  resolver output (slot allocation, cell-vars, upvalue plan, shapes)
        │
        ▼
   COMPILER  ─────►  Chunk { code: Vec<u8>, consts: Vec<Value>,
        │                    spans: Vec<(offset, Span)>, upvalue_descriptors, ic_count }
        ▼
   VM: async dispatch loop over FIBERS   (reuses value.rs + entire stdlib unchanged)
```

### The Fiber model (unifies async + generators)

A **Fiber** is one independent execution context:
```
struct Fiber { frames: Vec<CallFrame>, stack: Vec<Value>, state: Running | Suspended | Done }
struct CallFrame { closure: Rc<Closure>, ip: usize, slot_base: usize }
```
- The **main program**, each **generator**, and each **spawned async task** are all Fibers — in
  different drive harnesses. This deletes `coro.rs`'s `poll_fn` parking + thread-local current-generator
  stack: a Fiber *has* an explicit stack, so suspension is "return `Yielded`, keep the frames."
- **Explicit `frames: Vec<CallFrame>`** is what makes AScript recursion bounded by heap, not the Rust
  stack — a `CALL` pushes a frame and the Rust loop stays flat. Resolves a CLAUDE.md async non-goal
  (robust deep recursion) as a side effect.

### What is reused unchanged

- `src/value.rs` — the operand stack holds `Value`s. **One addition:** a `shape_id` on `Object`/
  `Instance` (see Specialization).
- The entire `src/stdlib/*` — native fns stay `Value → Value`; the VM calls them identically. Native
  *async* builtins (reqwest, sqlite) are awaited inline by the run loop.
- `src/task.rs` `SharedFuture` + `AbortHandle` cancel-on-drop; `std/task` spawn/gather/race/timeout.
- The `Control`/`Flow`/`AsError` error model and `recover` semantics.
- Type-contract / schema / `.from` / typed-parse semantics (invoked from the VM call path).

### What is replaced

- `Function { body: Vec<Stmt>, closure: Environment }` → `Rc<Closure>` (compiled `Chunk` + captured
  upvalues).
- The tree-walking `eval_expr`/`exec` → compiler emit-arms + VM execute-arms.

## Instruction set (stack-based)

**Format:** `u8` opcode + inline operands (`u8`/`u16` indices, `i16` jump offsets); one decode loop.
Specializable opcodes also carry a `u16` inline-cache index.

**Opcode families** (illustrative; the plan carries the exhaustive table):

| Family | Examples |
|---|---|
| stack/consts | `CONST`, `NIL`, `TRUE`, `FALSE`, `POP`, `DUP` |
| locals/upvalues/globals | `GET/SET_LOCAL`, `GET/SET_UPVALUE`, `CLOSE_UPVALUE`, `GET/SET_GLOBAL` |
| arithmetic/logic | `ADD`, `SUB`, `MUL`, `DIV`, `MOD`, `POW`, `NEG`, `NOT`, `EQ`, `LT`, `LE`, … |
| control flow | `JUMP`, `JUMP_IF_FALSE`, `JUMP_IF_TRUE`, `LOOP` |
| calls/returns | `CALL argc`, `RETURN`, `CLOSURE protoIdx [captures]` |
| objects/collections | `NEW_ARRAY n`, `NEW_OBJECT n`, `SPREAD`, `GET_INDEX`, `SET_INDEX`, `GET_PROP`, `SET_PROP`, **`GET_PROP_OPT`** (optional chaining `?.`) |
| classes/enums | `CLASS`, `METHOD`, `GET_SUPER`, `INSTANCE_OF` |
| strings | **`TEMPLATE n`** (build an interpolated string from n parts) |
| AScript-specific | `AWAIT`, `YIELD`, `MAKE_GENERATOR`, `PROPAGATE` (`?`), `UNWRAP` (`!`), `MATCH_*`, `IMPORT` |

**`ExprKind` → opcode coverage** is an explicit table in the implementation plan, so no AST node silently
lacks support. Notable mappings caught during design:
- `OptMember` (`?.`) → `GET_PROP_OPT` (nil receiver short-circuits to nil).
- `Template` → `TEMPLATE n` (coerce parts to string + concat).
- `Paren` → no opcode; affects only optional-chain breaking (compiler doesn't flatten it).
- **Schema fluent chaining + typed-parse + `.from` are NOT opcodes** — they're *call-path behaviors*:
  the `CALL`/method path replicates the current evaluator hook (`is_schema_value` + `is_schema_method`
  → `call_schema`; `Class.from`/`json.parse(_, Class)` are ordinary calls with a class argument).

**Data structures need no opcodes.** `Map`, `Set`, `Bytes`, `Regex`, `Decimal`, `Schema` have no literal
syntax — they are produced by stdlib **calls** and ride the operand stack as ordinary `Value`s. Only
`Array`/`Object` have literal syntax (`NEW_ARRAY`/`NEW_OBJECT`).

## Compiler

`src/compile/` — a single-pass recursive emitter that leans on the resolver so the VM never does name
lookups at runtime:
- **Locals → slot indices** (`GET/SET_LOCAL slot`); **captures → upvalues** (`CLOSURE` with the
  resolver's upvalue plan); **globals/builtins → `GET_GLOBAL`** (later IC-specialized).
- **Literals → constant pool**, parsed **once at compile time** (numbers/strings/decimals). This
  subsumes any notion of node-attached "value caching."
- **Span table** (`Vec<(offset, Span)>`) built **from day one** — a runtime panic binary-searches it to
  produce AScript's usual source-pointing ariadne diagnostics. Non-negotiable for diagnostics parity.
- **Disassembler** (`disasm(&Chunk) -> String`) built alongside — the primary debugging tool *and* the
  substrate for compiler unit tests (assert emitted opcodes by disassembly).

## Closures & upvalues

Upvalues, implemented Rust-idiomatically (no `unsafe`, no stack-pointer dance):
- Captured-and-mutated locals are stored as **`Rc<RefCell<Value>>` cells**; a closure captures *clones of
  the `Rc`* → shared by-reference, preserving AScript's current JS-like capture semantics. Non-captured
  locals stay plain `Value` in their slot.
- **Capture-by-value optimization (Luau-style) — LANDED (SP8 #136):** when the resolver proves a captured
  variable is never reassigned (`captured && !mutated`), it is captured by value (the `Value` is copied
  into the closure's own fresh cell at `Op::Closure`; the declaring frame keeps a plain stack local — no
  cell allocation, no per-access `RefCell` borrow) — faster closures, better locality. ~90% of captures
  are immutable. A reassigned capture keeps the shared by-reference cell.
- The **resolver** marks cell-vars and the per-closure upvalue plan; the compiler emits accordingly.

## Async/await & generators (model 2a — highest risk)

The run loop is **async** and owns its Fiber by `&mut` (not behind a `RefCell`):
```
enum RunOutcome { Done(Value), Yielded(Value) }
async fn run(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control>
```
- **`await` (`OP_AWAIT`):** pop a value; if `Value::Future`, drive its `SharedFuture` to completion
  inline (`.await`) and push the result; non-future → identity (matches M17). Because the Fiber is a
  `&mut` local (not stored behind a `RefCell` across the await), the "never hold a `RefCell` borrow
  across `.await`" invariant holds structurally; native resources keep the take-out-across-await
  pattern; `clippy::await_holding_refcell_ref` stays denied.
- **Calling a script `async fn`:** the `CALL` path detects `is_async`, creates a Fiber, **eagerly
  `spawn_local`s** a task that runs it and resolves a `SharedFuture`, and pushes `Value::Future`
  (M17 eager scheduling + cancel-on-drop; the handle owns the `AbortHandle`).
- **Native async builtins** are awaited inline at the `CALL` site (same suspension path).
- **Generators:** `MAKE_GENERATOR` packages a Suspended Fiber. `gen.next(v)`/`for await` call an async
  `resume(gen, input)` that drives `run` to the next `OP_YIELD` (→ `Some(v)`, frames intact) or
  completion (→ `None`). An `async fn*` can both `yield` and `await` internally. **No `poll_fn`, no
  thread-local current-generator stack** — the Fiber is the context.
- **Structured concurrency reused as-is:** `std/task` spawn/gather/race/timeout over `Value::Future`s
  (Fibers on spawned tasks); race/timeout cancel via dropping handles → `AbortHandle` aborts the Fiber;
  the `INFLIGHT_YIELD_CAP` reaper bounds un-awaited tasks; `http.serve` per-connection `spawn_local` +
  `Semaphore` cap unchanged.

## Error handling

`Flow`/`Control` reused unchanged (`Control` stays `Clone` so a panic rides a cross-task future). The VM
unwinds the **explicit Fiber frame stack** itself:
- **`?` (`PROPAGATE`):** pop `[value, err]`; `err == nil` → push `value`; else **function-level early
  return** of `[nil, err]` (unwind to frame boundary, close upvalues, resume in caller).
- **`!` (`UNWRAP`):** pop `[value, err]`; `err == nil` → push `value`; else raise a **recoverable**
  `Control::Panic` carrying `err`'s message.
- **Panic:** pop frames (closing upvalues) to the catch boundary; uncaught → `run()` returns
  `Err(Panic)` to the driver (top-level → ariadne diagnostic via the span table; spawned Fiber →
  resolve its `SharedFuture` with `Err`, re-emerging in the awaiter).
- **`recover` is a native builtin over `call_value`, not an opcode.** It stays a first-class function
  (`recover(fn)`); the native impl runs the closure via `call_value` and matches the outcome:
  `Err(Panic)` → `[nil, err]`; the `exit` Control **passes through unchanged**. (An opcode would change
  its first-class nature and wouldn't remove its dominant cost — the closure. See decision log.)
- **`call_value`** — `async fn call_value(&self, callee, args) -> Result<Value, Control>` — is the
  native↔VM callback primitive used by `recover` *and* every higher-order stdlib fn (`map`/`filter`/
  comparators/middleware), and is where async re-enters. Built early; load-bearing.
- **Diagnostics parity** (panic message + source location identical to the tree-walker) is a test gate.

> **Future extension point:** a closure-free `recover { ... }` *block* form would justify an
> exception-region opcode (handler table, zero-alloc, zero-cost-when-not-thrown) for hot-path recover.
> The VM can support both forms without conflict. Not built now; recorded so it isn't reinvented.

## Specialization & inline caching (in-spec; nothing core deferred)

The dynamic-language performance layer, built as a **phase after the base VM reaches parity**, semantics
preserved (guards + deopt), gated by `std/bench` and three-way differential testing.

- **Shapes (hidden classes), minimal-representation approach.** Add a **`shape_id`** to each `Object`/
  `Instance`; a **per-VM `ShapeRegistry`** assigns ids to key-layouts via a transition tree. `Object` is
  already an `IndexMap` (keys separate from values, stable insertion-ordered indices, O(1)
  `get_index_of`), so the IC caches `(shape_id, index)` and the fast path is
  `obj.shape_id == cached → values.get_index(cached_index)` — **no value-model rewrite**, full
  object+instance property-access caching (not deferred). A class gives instances a base shape from its
  declared fields. Mutation correctness is free: reassigning a field keeps the shape; adding/removing a
  key transitions it → IC misses and re-caches. *Per-VM registry, not global — isolate-friendly.*
- **Polymorphic inline caches** at `GET_PROP`/`SET_PROP`/`CALL_METHOD`: monomorphic → polymorphic
  (≤4 shapes, a short shape-test cascade) → **megamorphic** fallback to generic lookup. Cache data in a
  parallel `Vec<InlineCache>` on the `Chunk` (behind `Cell`/`RefCell`; `!Send` → no atomics).
- **PEP 659 adaptive specialization** for arithmetic (`ADD_NUMBER`/`ADD_DECIMAL`/`CONCAT_STR`) and
  `GET_GLOBAL_CACHED`: adaptive instruction families, warmup counter (first cache slot), quickening
  (rewrite opcode in place), deopt on guard miss.
- **AScript-semantic guards:** schema-value receivers and `?.` nil-receivers never take a cached fast
  path (guard excludes → generic).
- **Kill switch + three-way differential test:** `--no-specialize` runs pure-generic; the suite runs
  generic-VM == specialized-VM == tree-walker, byte-identical. A guard bug surfaces instantly.

> **Known host constraint (documented, with revisit trigger):** threaded dispatch (computed-goto /
> tail-call) needs labels-as-values or guaranteed tail calls — unavailable in stable Rust (`become`
> unstable). Dispatch stays a structured `match`-in-a-loop (LLVM often lowers to a jump table); revisit
> if/when Rust stabilizes `become`.

## Bytecode persistence (`.aso`)

- **Extension `.aso`** ("AScript Object"). It is **VM bytecode requiring the `ascript` runtime**, not a
  native executable.
- A `Chunk` is serializable (its const pool holds only compile-time literals + nested function protos —
  never runtime `Value`s). **Specialization is runtime-only and does not serialize** — the `.aso` holds
  the *generic* chunk, so the on-disk format is stable across IC/spec evolution.
- **Version magic header:** `.aso` is tied to a specific opcode set + value layout. A version mismatch
  **recompiles** (or errors) — never runs stale bytecode.
- **Trust model:** loading `.aso` runs its bytecode. Treat `.aso` as trusted input (like `.pyc`) and/or
  run a **bytecode verifier** on load (validate jump targets, stack-depth balance, operand ranges).
- **`ascript build foo.as → foo.aso`** command.
- **`import` resolves `.aso` modules** (feature A): the resolver finds `foo.aso` as well as `foo.as`,
  verifies the header, loads the chunk, runs its top-level, binds exports — behavior identical to
  importing source, minus the compile step. **Precedence:** prefer `.aso` when no source is present or
  the `.aso` is up-to-date vs source (Python's rule); else compile source. Transitive imports work.
  This is the `.pyc`/`.jar` model — **not** native `.so`/`.dll` linking (that is the deferred FFI spec).

## Testing — four oracles

1. **Differential vs tree-walker:** byte-identical stdout/exit-code over the whole `examples/` corpus and
   the full `cargo test` suite, in both feature configs.
2. **Recorded goldens** from `main` (Phase 9 `assert.snapshot`) — survive the tree-walker's deletion.
3. **Three-way specialization check:** generic-VM == specialized-VM == tree-walker (kill-switch enables).
4. **Unit tests:** compiler via the **disassembler** (assert opcodes); VM via hand-written bytecode;
   **diagnostics parity** (panic message + location vs tree-walker); bytecode **verifier** tests
   (reject malformed `.aso`).

## Cycle-collecting GC (final phase of this sub-project)

**Why it's needed (and not obviated by the VM):** the need comes from the **value model**, which the VM
reuses — AScript exposes mutable, shareable, `Rc`-backed containers (`Array`/`Object`/`Map`/`Set`/
`Instance`), so user code can form reference cycles (`let a=[]; a.push(a)`, mutually-referencing objects,
mutually-capturing closures) that pure `Rc` refcounting can never reclaim. The VM *adds* cycle-capable
structures (upvalue cells; Fiber↔`Future`/`Generator` handle loops), so it makes the GC **more**
relevant, not less. Harmless for short scripts (process exit reclaims), but fatal for long-running
servers — the same class of unbounded growth M17 fought.

**Approach: `gcmodule`'s `Cc<T>`** (refcounting + Bacon–Rajan trial-deletion cycle collection), chosen
(A1) over a hand-rolled tracing collector (B1). It **augments** refcounting rather than replacing it:
acyclic objects keep deterministic, immediate `Drop`; only actual cycles are trial-deleted. With `Cc`,
the operand stack/frames just hold ordinary `Cc` strong refs and external references are accounted for
by the refcount — **no separate root-enumeration API is needed** (this was a point in A1's favor).

**Build order (single migration):** the VM reaches behavioral parity on **`Rc` first** (keeps the
differential-correctness hunt clean, no `Cc` ergonomics in the bug-search). Then the **closing phase**:
- migrate cycle-capable `Value`s + the new upvalue cells + Fiber/Closure structures `Rc → Cc`, **once**,
  over the VM's *final* representation (no double-churn);
- implement `Trace` for all cycle-capable types (incl. `IndexMap`-backed `Object`/`Map`);
- enable + tune collection (scheduling/threshold).

**Soundness gates (distinct from the VM's behavioral oracle — this is the *memory* proof):**
- **Cycle reclamation** tests: `a.push(a)`, mutually-referencing objects/instances, mutually-capturing
  closures, Fiber↔`Future` loops — all collected, verified by RSS/heap assertions.
- **Long-running `http.serve` soak test:** bounded memory across many requests (no per-request leak).
- **Deterministic native-resource `Drop` preserved (critical):** TCP/files/DB connections/child
  processes/cancel-on-drop tasks must keep immediate, ordered cleanup. `Cc` preserves this for the
  acyclic case by construction; the gate explicitly verifies a native resource is never trapped in a
  collected cycle and its `Drop` timing is unchanged.

The differential-correctness oracle (VM == tree-walker) and this memory proof are kept in **separate
phases**, so a behavioral bug and a collector bug never share a phase.

## Performance gate

`std/bench` suite (deep recursion, tight loops, property access, string building, method dispatch). Gate:
a **real speed-up** (target **≥2× on compute-bound** code) with **no regression on any** benchmark.
Falling short triggers the IC/specialization work *before* cutover.

## Migration & rollout

- **Vertical-slice sequencing** (each = compiler arm + VM arm + differential test): sync core → control
  flow → functions/calls → closures/upvalues → error handling (`?`/`!`/`recover`) → `await` →
  generators → classes/enums/`super` → match/destructuring/spread → **then** shapes + ICs →
  specialization → `.aso` persistence + import → **finally `Rc → Cc` migration + cycle collector**
  (the GC phase; soundness/soak/resource-`Drop` gates).
- Build **alongside the tree-walker** (the differential oracle); the tree-walker keeps running the binary
  throughout. Async (await/generators) likely its **own sub-spec/plan** — risk concentration.
- **Cutover:** per the front-end spec's OPEN DECISION (default **(i)**): develop front-end #1 + VM #2 on
  one branch, **single merge**, delete the legacy lexer/parser/AST/**interp** together then — so `main`
  never carries two runtimes. `main` is frozen by the project owner until then; no drift.

## Future extension points (recorded, not built)

- **Level-1/Level-2 parallelism** — VM is isolate-friendly (no new globals/thread-locals; per-VM
  `ShapeRegistry`), so native-offload (L1) and isolate workers (L2) stay clean additive future phases.
- **`recover { }` block form + exception-region opcode** — for hot-path recover.
- **Native `.so`/`.dll` plugin FFI** — separate future spec (ABI, `libloading`, security model).
- **CacheIR-style structured IC IR** — north star, especially if a JIT is ever pursued.
- **Threaded dispatch** — revisit when stable Rust gains `become`.

## Decisions (log)

- **Runtime form:** bytecode compiler + VM (supersedes tree-walker); CST is tooling-only.
- **ISA:** stack-based (research: register's edge shrinks for dynamic languages; specialization, not
  ISA, is the modern win). Specialization is **in-spec**, not deferred.
- **Suspension model:** 2a (async dispatch loop borrowing tokio). 2b deferred (non-goal).
- **Fiber** unifies main/async/generator execution; explicit frame stack ⇒ deep recursion.
- **Closures:** upvalues via `Cc<RefCell<Value>>` cells; capture-by-value for immutable (never-reassigned)
  captures — LANDED in SP8 #136 — resolver-driven (`captured && !mutated`).
- **`recover`:** native builtin over `call_value` (stays first-class); no opcode (block-form opcode is a
  future option).
- **Specialization:** shapes via `shape_id`-on-`IndexMap` + per-VM `ShapeRegistry`; polymorphic ICs
  (≤4, megamorphic fallback); PEP 659 adaptive for arithmetic/globals; schema/`?.` guards; kill switch +
  three-way differential. Object property caching **not deferred**.
- **Dispatch:** structured `match` loop (threaded dispatch blocked by stable Rust — documented).
- **Bytecode files:** extension **`.aso`**; version header; trusted/verified; `ascript build`; **`import`
  resolves `.aso`** (feature A). Native dynamic-library FFI (B) = separate future spec.
- **Reuse:** `value.rs` (+ `shape_id`) and all of `stdlib/*` unchanged; `task.rs` cancel-on-drop reused.
- **Perf gate:** real speed-up (≥2× compute-bound target), no regression on any benchmark.
- **GC folded into this sub-project** (not separate): `gcmodule` `Cc` + trial deletion, as the **final
  phase** after VM parity on `Rc`; single `Rc→Cc` migration over the final representation; `Trace` for
  cycle-capable types; soundness/soak gates; **deterministic native-resource `Drop` preserved**.
