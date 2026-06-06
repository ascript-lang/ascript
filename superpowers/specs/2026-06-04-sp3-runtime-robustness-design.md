# SP3 — Runtime robustness (capacity panics → diagnostics; deep recursion → graceful) — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (gap register in the session handoff; SP1 done, SP2–SP10 around it).

**Goal:** Make the runtime *fail cleanly* on two classes of large-but-valid input that today crash the
process. (1) The bytecode compiler/`.aso` serializer **`panic!`/`.expect`** when a module exceeds an
internal bytecode capacity (constant pool / proto / class-proto / import table > `u16::MAX`, or a
serialized collection/byte-field > `u32::MAX`); these must become clean `CompileError`s / serialization
errors with an actionable message and a non-zero exit, never a panic. (2) Deeply nested non-yielding
recursion (and deeply nested expressions) overflow the **native** stack and `SIGABRT` (exit 134) on
both engines; a **recursion-depth guard** must convert the impending overflow into a clean, catchable
Tier-2 panic `maximum recursion depth exceeded` **before** the native stack blows, **identically on
both engines** so the whole-corpus differential stays byte-identical.

**Architecture:** Two focused changes. (A) Convert every capacity `panic!`/`.expect` in the bytecode
emit + serialize path (`src/vm/chunk.rs`, `src/vm/aso.rs`, with a sweep of `src/vm/{opcode,verify}.rs`
and the `src/compile/mod.rs` emit sites) into a typed error returned up the existing `CompileError` /
`AsoError` channels. (B) Add a single logical **call/eval depth counter** on the shared `Interp`
(`Cell<u32>`), incremented at the matching logical points in BOTH engines (the VM's call-frame push +
re-entrant `run`/eval recursion, and the tree-walker's `call_function`/`run_body` + nested
`eval_expr`), with one conservative shared limit. Over the limit → a Tier-2 `Control::Panic` with a
fixed message, at the same *logical* depth on both engines.

**Tech stack:** Rust. CST front-end → resolver → compiler (`src/compile/mod.rs`) → `Chunk` → VM
(default, `src/vm/*`); legacy front-end → tree-walker (`src/interp.rs`, the byte-identical reference
oracle, `ascript run --tree-walker`). gcmodule GC. `.aso` versioned bytecode (`src/vm/aso.rs`,
currently **v7** after SP1).

---

## Project invariants (hold for every task in this sub-project)

- **Two engines, byte-identical.** The bytecode VM (default) and the `--tree-walker` reference oracle
  must produce **byte-identical** stdout + exit on the whole-corpus three-way differential
  (`tree-walker == specialized-VM == generic-VM`) in `tests/vm_differential.rs`, plus the recorded
  goldens. A divergence on valid code is a root-cause bug — **never** weaken the assertion or edit a
  passing tree-walker test to match the VM.
- **Both feature configs green:** `cargo test` (default = full stdlib) AND `cargo test
  --no-default-features` (bare language), 0 failures across all binaries.
- **Clippy clean** under `cargo clippy --all-targets` AND `cargo clippy --no-default-features
  --all-targets`; `await_holding_refcell_ref = "deny"` stays in `Cargo.toml` and clean (the depth
  counter is a `Cell`, not a `RefCell`, so it cannot be held across an `.await`).
- **Perf gate:** geomean **≥2×** compute-bound vs the tree-walker, no specialized-vs-generic regression
  (`tests/vm_bench.rs`). The depth guard is a single `Cell` increment/decrement per call/eval — it must
  not regress the gate (measure; if a hot path shows up, gate the increment on the same `if
  self.specialize`-free common path, but the counter is unconditional because correctness depends on
  it).
- **No `unsafe`, no `#[allow(...)]`, no `#[ignore]`, no stubs/TODOs.** Every capacity site becomes a
  real typed error; the depth guard is fully implemented on both engines.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-phase review (re-read spec, re-run gates, adversarial hunt) before sign-off.

---

## Non-goals (explicitly out of SP3)

- **Unbounded / arbitrarily deep recursion** — making the engines tolerate millions of native frames.
  That needs a stackful-coroutine or explicit-stack VM and is the **SP9 architectural non-goal**
  (documented in spec §7 and the async-generators ADR). SP3 turns the crash into a *deterministic,
  catchable error at a fixed logical depth*, not into unlimited recursion.
- **Raising the per-platform native stack to match the chosen limit.** The limit is deliberately set
  *well below* native capacity (with margin for the largest per-frame future), so the residual gap
  between "the limit" and "raw native capacity" is by design (see §B6).
- **Tree-walker bytecode limits.** The tree-walker has no `u16`/`u32` bytecode capacities; the §A
  capacity errors are VM/compile/`.aso`-only. A program too large for the bytecode format fails cleanly
  on the VM and *runs* on the tree-walker (which has no such cap) — that asymmetry is correct and
  documented (§A5), because it is not a "parser-accepts-but-engine-rejects-valid-code" hole: the VM
  rejection is an honest capacity error, and the tree-walker is a debug/oracle engine.
- **Streaming/segmented bytecode, module splitting, wide (`u32`) operands.** Raising the actual
  capacities is a future codegen project; SP3 only makes hitting them clean.

---

## §A — Capacity panics → clean diagnostics

### A1 — Current behavior (verified, file:line)

Every site below is reached only by a **large-but-valid** program; each currently `panic!`s or
`.expect`s, unwinding to the binary's catch and (for an `.expect`) printing a Rust panic message + a
panic exit, NOT a clean ariadne diagnostic. The full inventory found by sweeping
`src/vm/{chunk,aso,opcode,verify}.rs` and the `src/compile/mod.rs` emit sites:

**`src/vm/chunk.rs` — pool/jump capacity `.expect`/`panic!` (the core of §A):**
- `:328` `add_const` — `u16::try_from(idx).expect("const pool exceeded u16::MAX")` — a module with
  > 65535 distinct constants.
- `:324` `add_const` — `u16::try_from(i).expect("const index fits u16")` on the dedup-hit return path
  (only reachable once the pool is already oversized; convert together with `:328`).
- `:339` `add_proto` — `.expect("proto table exceeded u16::MAX")` — > 65535 nested function protos.
- `:350` `add_class_proto` — `.expect("class-proto table exceeded u16::MAX")` — > 65535 classes.
- `:361` `add_import` — `.expect("import table exceeded u16::MAX")` — > 65535 imports.
- `:291–292` `emit_jump` and `:307–308` `emit_loop` — `i16::try_from(disp).unwrap_or_else(|_|
  panic!("jump/loop displacement out of i16 range"))` — a single function body whose forward jump or
  backward loop spans > 32 KB of bytecode (a huge single block / loop body). These are emitted from
  `src/compile/mod.rs`; the displacement is a function of generated code size, so a valid but enormous
  body triggers them.

**`src/vm/aso.rs` — serialization capacity `.expect` (the `.aso` build path):**
- `:236` `Writer::bytes` — `u32::try_from(b.len()).expect(".aso byte field exceeds u32::MAX")` — a
  single string/bytes literal > 4 GB (e.g. an enormous embedded blob).
- `:257` `Writer::len` — `u32::try_from(n).expect(".aso collection exceeds u32::MAX")` — any serialized
  table (consts/protos/code/spans/…) with > `u32::MAX` entries.

**`src/compile/mod.rs` — emit-path capacity (already clean — keep as the MODEL, audit for any missed):**
- `:693`, `:945`, `:955`, `:1096`, `:1103`, `:1477`, `:1604`, `:1610`, `:1698`, `:1816`, `:2030`,
  `:2082`, `:2120`, `:2230`, `:3395`, `:3411`, `:3518`, `:3525` already do
  `u16::try_from(...).map_err(|_| CompileError::new("…(max 65535)…", span))` (slot windows / cell+local
  indices / array fixed-length). `:2268` likewise for arity ("too many parameters (max 255)"). These
  are the **target shape**; the §A work makes the `chunk.rs`/`aso.rs` sites match them. The audit must
  confirm there is no *other* `as u8`/`as u16`/`.expect`/`panic!` capacity truncation on the emit path
  (e.g. `:886`/`:945`-adjacent fresh-cell collection uses `if let Ok(slot) = u16::try_from(...)`, a
  silent skip — verify it is genuinely unreachable past the slot-count guard at `:693`/`:2230`, or
  convert it; see A4).

**`src/vm/opcode.rs` / `src/vm/verify.rs`:** the sweep found **no production capacity panics** — the
only `panic!`/`assert!` there are in `#[cfg(test)]` blocks (`opcode.rs:756`; `verify.rs` test helpers
and the `verify.rs:749/790/800` test-only `unwrap_or_else(panic!)`). The verifier's own error path is
already typed (`VerifyError`, returned). **No production change in these two files** — they are in the
sweep scope only to *prove the negative* (and a regression guard test belongs here).

### A2 — Target semantics

- Each capacity site returns a typed error up the existing channel instead of panicking:
  - **`chunk.rs` pool/jump sites** surface as a `CompileError` (message + `Span`). The chunk builder
    (`add_const`/`add_proto`/`add_class_proto`/`add_import`/`emit_jump`/`emit_loop`) is called from the
    compiler, which already threads `Result<_, CompileError>`; these methods change signature to return
    `Result<u16, ChunkLimit>` (or set a sticky `overflow: Option<ChunkLimit>` flag on the `Chunk`
    checked once at `finish`, to avoid rippling `?` through hundreds of infallible emit calls — see A3
    for the chosen mechanism). The lib boundary converts `CompileError` → `AsError` → a clean ariadne
    diagnostic + non-zero exit, exactly as every other compile error does today.
  - **`aso.rs` writer sites** surface as an `AsoError` (a new `AsoError::TooLarge { what, len }`
    variant) returned from `to_bytes`/`write_chunk` (today `to_bytes` is infallible and `:353` does
    `write_chunk(...).expect("constant pool must be literals-only")` — that `.expect` stays for the
    genuine compiler invariant, but the capacity path returns the typed error). The `ascript build`
    command maps it to a clean message + non-zero exit.
- **Messages are actionable**, naming the limit and a remedy. Exact strings:
  - const pool: `"module exceeds 65535 constants; split the module into smaller files"`
  - proto table: `"module exceeds 65535 function definitions; split the module into smaller files"`
  - class-proto table: `"module exceeds 65535 class definitions; split the module into smaller files"`
  - import table: `"module exceeds 65535 imports; split the module into smaller files"`
  - jump/loop: `"function body too large to compile (a single jump exceeds 32 KB of bytecode); split it
    into smaller functions"`
  - `.aso` byte field: `"value too large to serialize (a single string or bytes literal exceeds 4 GB)"`
  - `.aso` collection: `"module too large to serialize (a table exceeds 4 billion entries); split the
    module into smaller files"`
- **Non-zero exit, never a panic / never exit 134.** The `ascript run`/`ascript build` paths return a
  normal error exit (the same code path a type error takes).

### A3 — Implementation mechanism (sticky overflow flag, chosen)

Threading `Result` through every `emit_*`/`add_*` call would touch ~hundreds of infallible call sites
and bloat the compiler. Instead:

- Add `overflow: Cell<Option<ChunkLimit>>` to `Chunk` (`ChunkLimit` is a small enum: `Consts`,
  `Protos`, `ClassProtos`, `Imports`, `Jump`, `Loop`, each carrying the triggering `Span` where one is
  available — pool sites use the chunk's current span via `record_span`, jump/loop sites already have a
  `Span`). When an `add_*`/`emit_*` would exceed its cap, it records the **first** overflow in
  `overflow` (sticky — first-wins, so the message points at the first offending construct) and returns
  a safe placeholder index (`u16::MAX`) / skips the emit; the bytecode is now known-invalid but the
  builder does not panic and continues without UB (the placeholder is never executed because compile
  aborts).
- The compiler's top-level `compile` / `compile_source` (and each nested-proto `finish`) checks
  `chunk.overflow` after building and, if set, returns `Err(CompileError::new(message(limit),
  span(limit)))`. This is the single check point; the per-site change is just "record + return
  placeholder" instead of "`.expect`".
- `aso.rs`: `Writer::bytes`/`Writer::len` return `Result<(), AsoError>`; `to_bytes` becomes
  `fn to_bytes(&self) -> Result<Vec<u8>, AsoError>` (callers in the `build` command + tests update). An
  alternative sticky-flag-on-`Writer` mirrors §A3's chunk approach if rippling `?` proves noisy — pick
  whichever keeps the diff smallest while staying panic-free (the implementer decides after reading the
  writer call graph; both are acceptable, neither panics).

### A4 — The silent-skip sites (`:886`, fresh-cell collection)

`compute_fresh_cells`-style sites (`:886`, `:945`-adjacent) do `if let Ok(slot) = u16::try_from(b.slot)
{ slots.push(slot) }` — a `slot` past `u16::MAX` is *silently dropped*, not panicked and not errored.
Past the `slot_count` guard at `:693`/`:2230` (which already errors "too many local slots … max
65535") this branch is unreachable for a slot ≥ 65536 (the frame would have been rejected first). The
audit must **prove** this (the slot-count guard runs before any fresh-cell emit for that frame) and, if
proven, leave a comment + a regression note; if NOT provably unreachable, convert it to record a
`ChunkLimit` so a dropped cell can never silently mis-compile. **No silent truncation may remain.**

### A5 — The documented engine asymmetry

A program that exceeds a bytecode capacity fails cleanly on the VM (a `CompileError`) but the
tree-walker has no bytecode and *runs* it. This is **not** a parity hole (SP1's invariant is "every
*grammar-accepted construct* runs on both engines" — not "every input of every *size* compiles"): the
VM rejection is an honest capacity error with a clear remedy, and the tree-walker is the debug/oracle
engine, not a second production dialect. The three-way differential corpus contains **no** module that
hits a bytecode cap (none is remotely close to 65535 constants), so the differential is unaffected.
This asymmetry is documented in §A (here), `CLAUDE.md`, and the build-command help.

### A6 — Tests

- **A generator-driven oversize test** (`tests/aso.rs` or a new `tests/vm_limits.rs`): a Rust test that
  programmatically emits an AScript source with > 65535 **distinct** constants (e.g.
  `"let _0=0.0\nlet _1=1.1\n…"` with 70 000 distinct number literals, or 70 000 distinct string
  literals — distinct so dedup does not collapse them), compiles it on the VM, and asserts the result
  is `Err(CompileError)` with the const-pool message — **not** a panic (the test would `SIGABRT` today).
  Run it under `#[should_panic]`-free assertion (`matches!(err, …)`), proving no panic.
- **Proto / class-proto / import** over-cap variants (generated source with > 65535 functions / classes
  / imports) — each asserts the matching clean error. (Import test may be slow; keep it `--release`-able
  or trimmed; do NOT `#[ignore]` it — gate it behind the same harness the const test uses.)
- **Jump/loop displacement:** a generated function body large enough to force a > 32 KB jump (a single
  block with enough straight-line statements) → the clean "function body too large" error.
- **`.aso` byte/collection caps:** these need > 4 GB inputs, which is impractical to materialize. Cover
  them with a **unit test on the `Writer` directly** (`src/vm/aso.rs` tests): call `Writer::bytes` /
  `Writer::len` with a length that exceeds `u32::MAX` via a fake length (do not allocate 4 GB — test the
  `try_from`-error path by constructing the error case, e.g. a helper that takes the length as a
  `usize` and asserts `Err(AsoError::TooLarge)`). No 4 GB allocation.
- **Negative-sweep guard** (`tests/vm_limits.rs`): a test asserting (by `grep`-style source scan in a
  build script OR a hand-maintained allow-list assertion) that `src/vm/{chunk,aso}.rs` contain **zero**
  non-`#[cfg(test)]` `.expect("…exceed…")` / `panic!("…range…")` capacity sites after the change. Keep
  it lightweight (a string-search over the file contents in the test) so a future capacity `.expect`
  re-introduction trips it.
- **Exit code:** a `tests/cli.rs` case running the oversize const program through the built binary and
  asserting a **non-134** non-zero exit + the actionable message on stderr.

---

## §B — Deep-recursion divergence → graceful, deterministic, documented

### B1 — Current behavior (verified, file:line + empirical)

Both engines are built on `#[async_recursion]` futures, so a recursive script call deepens the
**native** stack and `SIGABRT`s (exit **134**) past the per-frame budget. Empirically (debug build,
`fn f(n){ if(n<=0){return 0} return 1+f(n-1) }`):

- **Tree-walker** overflows between **100 and 200** logical frames (n=100 prints `100`; n=200 prints
  `fatal runtime error: stack overflow, aborting`, exit 134). Its call chain is `call_function` →
  `run_body` → `exec` → `eval_expr` — all `#[async_recursion(?Send)]` with very large frames
  (`src/interp.rs:1366 eval_expr`, `:2302 run_body`, `:2338 call_function`, plus the many
  `#[async_recursion]` helpers at `:865/963/974/1657/1808/1893/2041/2091/2123/2205/2301/2337/2405/2422/2480/2574/2653/2705/2722/2888`).
- **VM (default)** survives **1,000,000** straight recursive *compiled* calls and **5,000,000**
  recursive *method* calls — because an in-VM compiled call **pushes a `CallFrame` onto the heap-backed
  `fiber.frames` Vec and continues the run loop** (`src/vm/run.rs:994` `fiber.frames.push(...)`), NOT a
  native recursion. BUT the VM still SIGABRTs (exit 134, **empirically confirmed**) on:
  - **deeply nested expressions** — its compiler/eval is `#[async_recursion]`
    (`compile_expr`/`eval_chain`); a `let x = ((((…1…))))` with ~50 000 nested parens crashes
    (n=2 000 is fine, n=50 000 → exit 134), tree-walker too.
  - **the native re-entry call paths** — `invoke_compiled_method` (`src/vm/run.rs:3414` `self.run(&mut
    fiber).await`) and the `call_value` "other"-callee branch (`:1026`) **re-enter `Vm::run`** (a fresh
    `#[async_recursion]` frame at `src/vm/run.rs:253/429`). The hot `self.f(...)` IC fast path avoids
    this (frame push only), so straight recursion does not overflow; but dispatch paths that route
    through `invoke_compiled_method`/`call_value` (e.g. non-IC method dispatch, native-callee
    indirection, `recover`-wrapped calls) DO grow the native stack and can overflow far sooner than the
    frame-Vec path.

So the two engines overflow at **wildly different** native depths (tree-walker ~150, VM ~hundreds of
thousands for the frame-Vec path, but a few thousand for the native-re-entry path) — a real divergence
on the same program, and always a `SIGABRT` rather than a catchable error.

### B2 — Target semantics

A single, conservative, **logical** recursion limit `MAX_CALL_DEPTH`, identical on both engines. When a
call (or a nested expression evaluation — see B4) would push the **logical** depth past the limit, the
engine raises a **Tier-2 `Control::Panic`** with the fixed message **`maximum recursion depth
exceeded`** (anchored at the offending call/expression span), **before** the native stack overflows.
Because it is a `Control::Panic`, it:
- aborts the program with the normal recoverable-panic exit (a clean diagnostic, **not** exit 134), and
- is reported identically by both engines: same message, same logical depth → **byte-identical** stdout
  + exit on the differential.

The limit is set high enough that **no corpus program and no realistic program** reaches it (the corpus
max recursion is trivially small), so the whole-corpus three-way differential is **unchanged**
(byte-identical, no new divergence). A program written **at/over** the limit errors **identically** on
both engines (the dedicated B-phase differential tests assert this).

### B3 — Where the counter lives (chosen: a `Cell<u32>` on the shared `Interp`)

The `Vm` holds an `Rc<Interp>` (`src/vm/run.rs:52 interp: Rc<Interp>`), and the tree-walker *is* the
`Interp`. A **single** `call_depth: Cell<u32>` on `Interp` (`src/interp.rs`, beside `inflight:
Cell<u64>` at `:305`) is the one source of truth, shared by both engines, so the limit is provably
identical and the two engines cannot drift the constant. `Cell` (not `RefCell`) so it is never held
across an `.await` (`await_holding_refcell_ref` stays satisfied trivially).

**The counter is incremented at the matching LOGICAL points so both engines hit the limit at the same
logical depth** (this is the crux of staying byte-identical):

- **A "call" = one user-level function/method/constructor invocation.** Increment on entry, decrement
  on exit, via an RAII guard (`DepthGuard` that decrements on `Drop`, so it unwinds correctly through a
  `?`/panic). This is the single shared definition both engines use.
- **Tree-walker:** increment in **`run_body`** (`src/interp.rs:2302`) — the one funnel every script
  call (function, method, generator step body, async body) passes through to bind args + `exec` the
  body. `run_body` is `#[async_recursion]` and is the actual native-stack deepener, so guarding it
  bounds the native recursion. (Do NOT also count `call_function`/`invoke_method` separately — they
  delegate to `run_body`; counting only `run_body` gives one increment per logical call.)
- **VM:** increment per **`CallFrame` push** — both the in-loop push (`src/vm/run.rs:994`, the frame-Vec
  path) AND the native-re-entry constructors (`invoke_compiled_method` at `:3392 Fiber::new` + `:3414
  self.run`, and any `call_value`-routed VM call). Concretely: increment when a new logical call frame
  is established (frame push / `invoke_compiled_method` entry), decrement on RETURN / method-call
  return — so the VM's logical depth = `fiber.frames` depth across the whole fiber stack, counting
  re-entrant `run`s too. The frame-Vec push and the native re-entry both add exactly **one** to the
  logical depth, matching one tree-walker `run_body`.

Because "one logical call" maps to "one `run_body`" on the tree-walker and "one frame push" on the VM,
and both increment the **same** `Interp.call_depth`, a program of logical call-depth `D` trips the limit
at the same `D` on both engines → byte-identical.

### B4 — Nested-expression depth (the VM-and-tree-walker compile/eval recursion)

Deeply nested *expressions* (`((((…))))`, deeply nested binary chains) overflow via the
`#[async_recursion]` expr **evaluator** (tree-walker `eval_expr` `:1366`) and the VM expr **compiler**
(`compile_expr`/`eval_chain`). These are not "calls", so the call counter does not cover them.

**Decision:** the same `call_depth` counter is **also** incremented per nested `eval_expr` recursion on
the tree-walker and per nested `compile_expr` recursion on the VM compiler, using the SAME limit — i.e.
the guard counts *logical recursion depth* (calls + nested-expression nesting), not just calls. To keep
the engines byte-identical here:

- The natural unit is **AST/CST expression-nesting depth**, which is a property of the *source*, not of
  either engine's internals — both engines see the same nesting. The tree-walker increments in
  `eval_expr`; the VM increments in `compile_expr`. A source with expression-nesting depth `E` trips at
  the same `E` on both.
- **Caveat (open question O1, see below):** the tree-walker evaluates expressions at *runtime* while
  the VM nests at *compile time*. For a *pure-nesting* program (`let x=((((1))))`) both nest to the same
  `E` and trip identically. But a program that builds deep nesting *dynamically at runtime* (only
  possible via recursion, which the call counter already bounds) cannot create unbounded *static* expr
  nesting — static expr nesting is fixed by the source. So counting static expr nesting on each engine
  is byte-identical for the failing case (a literally deeply-nested source) and the differential holds.
  The owner should confirm we are comfortable with the VM tripping this at **compile** time (a
  `CompileError`-flavored panic) while the tree-walker trips it at **eval** time (a runtime
  `Control::Panic`) — both abort with the same message + non-134 exit, but one is "compile-ish" and one
  is "runtime". Recommendation: surface BOTH as the same `Control::Panic` "maximum recursion depth
  exceeded" so stdout+exit are byte-identical regardless of phase. (The VM's compile-time trip is
  wrapped into the same `Control::Panic` at the lib boundary so the observable result matches.)

### B5 — Catchability by `recover` (decision)

`maximum recursion depth exceeded` is a `Control::Panic`, so `recover(() => deeplyRecursive())`
(`src/interp.rs:2756`) WOULD catch it. **Decision: it IS catchable** (consistent with every other
Tier-2 panic; `recover`'s contract is "catch any Tier-2 panic except `exit`"). Two safeguards make this
safe:
- The `DepthGuard` decrements on unwind, so by the time `recover` regains control the depth is back
  down — a subsequent recursive call inside the `recover` handler starts from the caller's depth, not
  the blown depth.
- `recover` itself does one `call_value` (one logical call), so it costs one depth unit; the limit has
  enough margin (B6) that catching-and-retrying does not immediately re-trip from `recover`'s own frame.

This is asserted by a differential test (`recover` around an over-limit call yields `[nil, err]` with
`err.message == "maximum recursion depth exceeded"`, byte-identical on both engines).

### B6 — The limit value (chosen) + the residual-divergence documentation

- **`MAX_CALL_DEPTH = 3000`** logical units (calls + expr-nesting), a single `const` in a shared
  location (`src/interp.rs`, referenced by the VM). Rationale: the tree-walker's *native* overflow is at
  ~150 frames **in a debug build with the default 8 MB main-thread stack**, and the binary already runs
  on a `current_thread` tokio runtime. **3000 is ABOVE the debug-build native overflow** for the
  tree-walker — therefore the binary MUST run the program on a thread with an explicitly enlarged stack
  so that 3000 logical frames fit *under* native capacity with margin. **Decision:** the entry points
  spawn the program on a stack-sized worker (e.g. a `std::thread` with an 64 MB stack hosting the
  `current_thread` runtime + `LocalSet`, OR `tokio::runtime::Builder…thread_stack_size`), sized so 3000
  tree-walker `run_body` frames (the largest per-frame budget of either engine) fit with ≥2× headroom.
  The limit (3000) and the stack size (≥64 MB) are chosen together and verified by a test that runs a
  2999-deep recursion to completion (no overflow) and a 3001-deep recursion to the clean panic (no
  SIGABRT) on **both** engines. If 3000 cannot fit even in 64 MB for the tree-walker's huge async
  frames, lower the limit (e.g. 1500) — the limit is whatever value provably fits with margin; the
  *exact* number is chosen empirically in the implementing task by bisecting under the enlarged stack,
  and recorded here once measured. **The two engines share the final constant**, so they stay
  byte-identical regardless of the value picked.
- **Residual architectural divergence (documented, not a bug):** the limit (3000, or whatever is
  measured) is **far below** the VM's frame-Vec native capacity (hundreds of thousands) and somewhat
  above the tree-walker's raw native capacity (~150) — so the limit is a *uniform logical cap* that
  trades the VM's larger headroom for byte-identical behavior. Truly unbounded recursion stays the
  **SP9 non-goal** (needs an explicit-stack VM / stackful coroutines). This is recorded in spec §7, the
  async-generators ADR neighborhood, and `CLAUDE.md`.

### B7 — Tests

- **Differential** (`tests/vm_differential.rs`), byte-identical VM (spec+generic) vs tree-walker:
  - `recursion_at_limit_ok`: a recursion to depth `MAX_CALL_DEPTH - 1` returns normally, identical
    output both engines.
  - `recursion_over_limit_panics`: a recursion to depth `MAX_CALL_DEPTH + 1` → both engines emit
    `maximum recursion depth exceeded` (same message, same non-134 exit), byte-identical.
  - `nested_expr_over_limit_panics`: a source with expression-nesting > the limit → both engines emit
    the same panic (B4), byte-identical.
  - `recover_catches_recursion_limit`: `recover(() => f(BIG))` → `[nil, err]` with the fixed message,
    byte-identical (B5).
  - `mutual_recursion_over_limit`: `fn a(n){…b(n-1)…}` / `fn b(n){…a(n-1)…}` to over-limit → identical
    panic (proves the counter is per-logical-call, not per-function).
- **No-SIGABRT guard** (`tests/cli.rs`): run the over-limit program through the built binary and assert
  exit is the clean recoverable-panic code, **not 134**, with the message on stderr — and a 2999-deep
  program exits 0. Do this for **both** `ascript run` (VM) and `ascript run --tree-walker`.
- **Margin guard** (a unit test under the enlarged stack): the largest-budget frame (tree-walker
  `run_body`) at `MAX_CALL_DEPTH` does not overflow (runs to the clean panic), proving the stack size /
  limit pair has headroom.
- **Whole-corpus differential unchanged:** the existing three-way corpus + goldens stay byte-identical
  (the limit is far above any corpus recursion) — the standing gate, re-run.

---

## Testing & quality bar (whole sub-project)

- **Differential oracle never relaxed:** whole-corpus three-way (tree-walker == specialized-VM ==
  generic-VM) byte-identical, plus goldens, plus the new §A capacity + §B recursion tests. Any
  divergence on valid code = fix the root cause.
- **Both feature configs:** `cargo test` green default AND `--no-default-features` (the depth guard +
  capacity errors are CORE — they must build and pass under `--no-default-features`).
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`;
  `await_holding_refcell_ref` stays denied + clean.
- **Perf gate:** geomean ≥2× compute-bound, no spec-vs-generic regression (`tests/vm_bench.rs`) — the
  depth `Cell` inc/dec per call must not regress it (measure before/after).
- **No `unsafe`/`#[allow]`/`#[ignore]`/stubs/TODOs.**
- **Per-task commit** with the trailer. Independent per-phase review (re-read spec, re-run gates,
  adversarial hunt: oversize-module shapes; over-limit recursion on both engines incl. methods, mutual
  recursion, `recover`-wrapped, async functions, generator bodies).
- **Docs:** `CLAUDE.md` (the capacity-error asymmetry §A5 + the recursion-limit + the residual-divergence
  SP9 note); the language spec (`docs/superpowers/specs/2026-05-29-ascript-design.md`) error-model
  section (Tier-2 `maximum recursion depth exceeded`); `docs/content` error-handling page (mention the
  recursion limit + `recover`).

## Open design questions for the owner

- **O1 — VM expr-nesting trips at compile time, tree-walker at eval time (§B4).** Recommendation:
  surface both as the same `Control::Panic` "maximum recursion depth exceeded" so the observable result
  is byte-identical. Confirm acceptable (vs. the VM emitting a distinct compile-time message for the
  expr-nesting case — which would diverge from the tree-walker and is therefore NOT recommended).
- **O2 — the exact `MAX_CALL_DEPTH` constant and the worker stack size (§B6).** The design fixes the
  *method* (single shared constant, enlarged worker stack, bisect-to-fit-with-margin) but the precise
  numbers (proposed 3000 / ≥64 MB) are pinned empirically in the implementing task and recorded back
  here. Owner sign-off on the *approach* (enlarged stack so a non-trivial limit fits) vs. a much smaller
  limit on the stock stack.
- **O3 — `.aso` writer error-threading shape (§A3).** `to_bytes` returning `Result` (clean, ripples to
  a handful of callers) vs. a sticky `Writer.overflow` flag (smaller diff, mirrors the chunk approach).
  Both are panic-free; recommend whichever yields the smaller diff after reading the writer call graph —
  flagged so the owner is aware a public-ish signature may change.
