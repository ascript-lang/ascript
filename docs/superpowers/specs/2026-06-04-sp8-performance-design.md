# SP8 — Performance: recover the global-access regression + capture-by-value — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (SP1–SP10). This is the perf sub-project.

**Goal:** Recover the geomean regression (`~2.92×` at V11-T6 → `~2.69×` measured today on this
branch) introduced when module top-level `let/const/fn/class` became late-bound USER-GLOBALS, and
land the still-deferred **#136 capture-by-value** closure optimization. Both are pure speed changes:
the whole-corpus three-way differential (tree-walker == specialized-VM == generic-VM) must stay
**byte-identical**.

**Architecture:** Two focused, independent optimizations across the VM run loop (`src/vm/run.rs`),
the chunk-level inline-cache side-tables + adaptive state (`src/vm/chunk.rs`, `src/vm/adapt.rs`), the
resolver (`src/syntax/resolve/{mod,types}.rs`), and the compiler (`src/compile/mod.rs`). Each is
gated by the existing three-way differential staying byte-identical plus a perf-measurement task in
`tests/vm_bench.rs`. Neither touches `src/value.rs` or the tree-walker (`src/interp.rs`).

**Tech stack:** Rust. CST front-end → resolver → compiler → `Chunk` → VM (default, specializing);
the same VM run with `specialize = false` (generic) and the tree-walker are the byte-identical
oracles. gcmodule GC. `.aso` versioned bytecode.

---

## Background: why the regression exists (verified against the code + a bench run)

When a `.as` program's DIRECT-child top-level `let`/`const`/`fn`/`class`/`enum`/`import` binding
became a **module-scope user-global** (late-bound, to reproduce the tree-walker's single shared
module `Environment` with forward references), those bindings stopped being file-frame slot-locals:

- The resolver classifies a top-level decl as a global (`Binding.is_global`,
  `src/syntax/resolve/types.rs:47-53`); its references resolve to `Resolution::Global(name)`
  (`src/syntax/resolve/mod.rs:363-380`) and its decl site is recorded in `global_decl_ranges`
  (`types.rs:95-102`).
- The compiler lowers a global decl to `DEFINE_GLOBAL name` and every reference to
  `GET_GLOBAL name` / `SET_GLOBAL name` (`src/compile/mod.rs:2871-2881`, `:968-970`, `:1114`).
- At runtime, `Op::GetGlobal` (`src/vm/run.rs:627-685`) reads the value through a **string-keyed
  `IndexMap` lookup** (`get_user_global`, `src/vm/run.rs:3023-3025`) on EVERY execution. `Op::SetGlobal`
  (`:719-773`) writes through `update_user_global` (`:3040-3047`).

The V11-T4 `GET_GLOBAL` inline cache (`GlobalCache`, `src/vm/adapt.rs:155-193`; storage
`Chunk::global_caches`, `src/vm/chunk.rs:203`, `:438-448`) is **deliberately restricted to immutable
bare builtins**: the user-global branch (`src/vm/run.rs:661-668`) explicitly does NOT cache, with the
comment that caching the *value* would (a) thrash — every `SET_GLOBAL` would have to bump the global
version and invalidate the entry each loop iteration — and (b) risk serving a stale value. So a hot
top-level `let` reassigned in a loop pays a full string-keyed `IndexMap` get on every read.

**The hot benches that regressed are exactly the global-heavy ones** (`tests/vm_bench.rs:182-215`).
Their top-level `let`s (`sum`, `i`, `o`, `c`, `total`) are now user-globals, so the loop bodies do
`GET_GLOBAL`/`SET_GLOBAL` per iteration:

| bench | hot-loop body | V11-T6 spec/tw | today spec/tw |
|---|---|---|---|
| `numeric loop (1e6)` | `sum = sum + i` (sum: global) | 2.84× | 2.67× |
| `while loop (1e6)` | `sum = sum + i; i = i + 1` (both global) | 4.61× | 3.26× |
| `sum recursion (500 x2000)` | `total = total + sumto(500)` (total: global) | 5.83× | 5.14× |
| `property r/w (1e6)` | `o.x = o.x + o.y` (o: global, read each iter) | 3.08× | 2.99× |
| geomean spec/tw | — | **2.92×** | **2.69×** |

(`numeric loop`/`property r/w` regressed less because their bodies are dominated by the `for`-range
iterator's frame-LOCAL index, not the global; `while loop` regressed most — both `i` and `sum` are
globals read+written twice per iteration.)

---

## §1 — Global-access fast path: index-stable + structural generation (recovers the regression)

### Mechanism

The fix exploits an `IndexMap` invariant: **an inserted entry's positional index never changes**
(`IndexMap` only appends; `swap_remove`/`shift_remove` would move indices, but user-globals are
NEVER removed — `define_user_global` only `insert`s, `src/vm/run.rs:3052-3060`, and there is no
removal path). So once a global is defined, its slot in the map is a **stable array index** reachable
in O(1) via `IndexMap::get_index` / `get_index_mut`.

Add a per-site cache for `GET_GLOBAL` (and symmetrically `SET_GLOBAL`) that warms to a **direct
index read** instead of a string hash:

1. **Cache shape (new variant on `GlobalCache`, `src/vm/adapt.rs`):** beside the existing
   `Cold` / `Cached { value, version }` (builtins), add
   `IndexBound { idx: usize, struct_gen: u64 }` — the resolved user-global's stable map index plus
   the **structural generation** it was resolved at.
2. **Structural generation (`struct_gen`, new `Cell<u64>` on `Vm`, beside `global_version`,
   `src/vm/run.rs:136`):** bumped **ONLY when a global is DEFINED/inserted** (`define_user_global`),
   **NOT on a plain `SET_GLOBAL`/reassignment** (`update_user_global`). Inserting a new global is the
   only event that can change the name→index mapping or introduce a shadow; a value reassignment
   leaves every index intact. This is the crux: a hot reassigned-top-level-`let` loop NEVER bumps
   `struct_gen`, so the index cache stays hot across the entire loop — **no thrash**.
3. **`GET_GLOBAL` hit path:** if `self.specialize` and the site is `IndexBound { idx, struct_gen }`
   with `struct_gen == self.struct_gen`, read `self.user_globals.borrow().get_index(idx)` → the
   `(name, GlobalSlot)` pair, and push `slot.value.clone()`. The read is a plain `Vec`-backed index
   into the `IndexMap`'s entries — no hashing, no string compare.
4. **`GET_GLOBAL` cold/miss path:** the existing resolution order is unchanged
   (`src/vm/run.rs:639-684`): user-globals first (now via `get_full(name)` to obtain BOTH the value
   and its index), then immutable builtins (still cached as today's `Cached { value, version }`),
   then the `undefined variable` panic. On a user-global resolve, record
   `IndexBound { idx, struct_gen }`. A `struct_gen` miss (a new global was defined since) re-resolves
   by name and re-records the (possibly new) index.
5. **`SET_GLOBAL` fast path:** today `Op::SetGlobal` (`src/vm/run.rs:719-773`) does
   `user_global_mutable(name)` (a string-keyed get) + `update_user_global(name, …)` (another
   string-keyed get). Cache the same `IndexBound { idx, struct_gen }` at the `SET_GLOBAL` site: on a
   hit, `get_index_mut(idx)` once, verify mutability (the `GlobalSlot.mutable` flag at that index),
   write `slot.value` in place. The cross-chunk immutable-error and undefined-variable paths are
   unchanged (they fall through on the same conditions). `SET_GLOBAL` continues to NOT bump
   `struct_gen` (it is not a define), so the GET cache for the same name stays hot.

### Why it is byte-identical

- The fast path returns/writes **the same `Value` the generic path would** — it reaches the same
  `GlobalSlot` (the index resolves to the identical entry the name would). The only difference is the
  lookup is by stable index instead of by string hash.
- **Resolution order is preserved.** The index cache is consulted ONLY after confirming (at cold-fill
  time) that the name resolved to a *user-global* (not a builtin, not undefined). A builtin name
  still takes the existing `Cached { value, version }` path; an undefined name still panics. A
  user-global that SHADOWS a builtin still wins, because the cold fill resolves user-globals first
  (unchanged).
- **Shadowing / redefinition safety via `struct_gen`.** The only events that can change which entry a
  name maps to — a new `DEFINE_GLOBAL` (including a redeclaration, REPL line-to-line, or an import
  binding) — bump `struct_gen`, invalidating every `IndexBound` entry. After such an event the site
  re-resolves by name. A redeclaration still hits its runtime
  `'<name>' is already defined in this scope` error on the SECOND `DEFINE_GLOBAL`
  (`src/vm/run.rs:711-715`) before any cache is consulted.
- **Mutability + cross-chunk immutable errors unchanged.** `SET_GLOBAL`'s mutability decision still
  reads the `GlobalSlot.mutable` flag (now at the cached index instead of by name); an immutable
  global reassigned from a later chunk still errors `cannot assign to immutable binding '<n>'` at the
  same point with the same message (`src/vm/run.rs:756-771`).
- **KILL SWITCH:** when `specialize == false`, the index cache is NEITHER consulted NOR recorded
  (gated on `self.specialize` exactly like the existing builtin cache, `src/vm/run.rs:659`), so the
  generic VM keeps doing the string-keyed lookup. The two modes stay byte-identical (same value, only
  faster) — the three-way differential enforces this.

### Why no thrash (the core design property)

The original V11-T4 restriction existed because **value caching + a version that bumps on every
write** forces per-iteration invalidation in a reassignment loop. This design breaks that coupling:

- We cache the **index**, not the value. An in-place value reassignment does not move the index, so
  the cached index stays valid.
- `struct_gen` bumps ONLY on DEFINE (insertion), never on SET (reassignment). The hot benches
  (`while loop`, `numeric loop`, `sum recursion`) only ever SET their globals inside the loop —
  `struct_gen` is stable for the loop's entire duration, so the index cache hits every iteration.

### Expected win

Each `GET_GLOBAL`/`SET_GLOBAL` in a hot loop drops from a string-hash `IndexMap::get`/`get_mut` to a
single bounds-checked `get_index`/`get_index_mut` plus a `u64` generation compare. Targeting recovery
of the regressed benches toward their V11-T6 figures (`while loop` ~4.6×, `numeric loop` ~2.8×, `sum
recursion` ~5.8×) and the geomean back to **≥ ~2.9×**, or as close as the design allows. The achieved
number is recorded in the plan's measurement gate. The `≥2×` compute-bound gate is never relaxed.

### Approach considered + rejected: per-global generation counters

An alternative is a per-global `generation: u64` on each `GlobalSlot`, bumped on that global's own
define, with the cache storing `(idx, that_global's_gen)`. This is finer-grained (defining a NEW
global B does not invalidate A's cache) but costs an extra field per slot and a generation read on
every hit (vs one shared `struct_gen` compare). Because global DEFINEs are rare (top-level decls run
once at load, then the set is stable — see `src/vm/run.rs:3006-3007`), the coarse shared `struct_gen`
invalidates caches essentially only at load time; after the top-level prologue it never bumps. The
shared `struct_gen` is therefore chosen for simplicity with no measurable downside; the per-global
counter is documented as the fallback if a pathological define-in-a-loop program ever shows up (none
exists in the corpus — defining a global inside a loop is a redeclaration error on the 2nd iteration).

### Edge cases (must stay byte-identical, covered by tests)

- A top-level `let` reassigned in a hot loop (the regression target) — index cache hits every
  iteration, value is correct, NO `struct_gen` bump.
- A function that reads a top-level `let` defined LATER in source (forward/late binding) — the cold
  fill resolves by name once the global exists; a call before the define still panics
  `undefined variable` identically.
- A redeclaration (`let x; let x`) — 2nd `DEFINE_GLOBAL` errors before any cache read; `struct_gen`
  bumped on the 1st define invalidates a between-defines cache.
- An immutable global (`const`/`fn`/`class`) reassigned (same chunk → compile-time `IMMUTABLE_ERROR`;
  cross-chunk → runtime `cannot assign to immutable binding`) — both unchanged.
- A user-global that shadows a builtin (`import { test }`) — user-global resolved first, cached by
  index; the builtin is never reached.
- REPL line-to-line: a new line's `DEFINE_GLOBAL` bumps `struct_gen`, so a prior line's compiled chunk
  re-resolves its globals by name on next execution (REPL re-runs a fresh chunk per line anyway).

---

## §2 — #136 capture-by-value closure optimization (additive fast path)

### Current behavior (verified)

The V5 closure model captures **every** captured local by REFERENCE through a heap cell. The resolver
comment is explicit (`src/syntax/resolve/mod.rs:519-529`): *"Baseline VM semantics: EVERY captured
local is a by-reference cell … Capture-by-value for never-forward-referenced immutable bindings is a
FUTURE optimization (V5), not the baseline."* Concretely:

- `cell_slots` = every binding with `captured == true` (`src/syntax/resolve/mod.rs:524-529`, and the
  same in the field-default frame `:787-791`).
- A cell is a `Cc<RefCell<Value>>` (`src/vm/value_ext.rs:16,25`; `Closure.upvalues:
  Vec<Cc<RefCell<Value>>>`), allocated nil at frame entry (`alloc_cells`, `src/vm/fiber.rs:49-60`)
  and filled when the declaration executes.
- A cell-slot read/write is `GET_LOCAL_CELL`/`SET_LOCAL_CELL` (compiler choice via `cur_cells`,
  `src/compile/mod.rs:839-855`; run loop `src/vm/run.rs:1600-1608`), each a `borrow()`/`borrow_mut()`
  + clone through the cell.
- `Op::Closure` captures by cloning the cell `Cc` (`src/vm/run.rs:1574-1593`); the closure reads it
  via `GET_UPVALUE` = `upvalues[idx].borrow().clone()` (`src/vm/run.rs:2013-2016`).

So even a closure that captures a constant it never reassigns pays: a cell allocation at frame entry,
a `RefCell` borrow on every access in the declaring frame, and a `RefCell` borrow on every upvalue
read in the closure.

### Mechanism

The optimization (already designed in `docs/superpowers/plans/2026-06-02-vm-V5-closures-upvalues.md`
T2 and the VM spec `docs/superpowers/specs/2026-06-02-bytecode-vm-design.md:147,327`, deferred at
ship): a variable that is **captured but NEVER reassigned after capture** can be captured BY VALUE
(the `Value` is copied into the upvalue) instead of through a shared heap cell.

The resolver already tracks reassignment: `Binding.mutated` (`src/syntax/resolve/types.rs:37`) is set
by `mark_mutated_target` (`src/syntax/resolve/mod.rs:851-862`) for any binding that is an
assignment TARGET anywhere. So the eligibility predicate is exactly **`captured && !mutated`**.

**Resolver changes (`src/syntax/resolve/{mod,types.rs}`):**

1. Narrow `cell_slots` to `captured && mutated` (remove a never-reassigned captured binding from the
   cell set — it will be captured by value instead). Two sites: `resolve_file` (`mod.rs:524-529`) and
   the field-default frame (`:787-791`). Add a parallel `value_capture_slots: Vec<u32>` =
   `captured && !mutated` to `FrameInfo` (`types.rs:62-67`), so the compiler/VM know which captured
   slots are plain values, not cells.
2. Tag each `UpvalueDescriptor` with whether the captured source is a value or a cell. Add
   `by_value: bool` to `UpvalueDescriptor::ParentLocal` (a transitive `ParentUpvalue` inherits its
   source's kind — capture from an upvalue keeps that upvalue's existing representation). The bit is
   derived from the source binding's `mutated` flag at `resolve_upvalue` time
   (`src/syntax/resolve/mod.rs:334-351`).

**Compiler changes (`src/compile/mod.rs`):** a `captured && !mutated` slot is NOT in `cur_cells`, so
`emit_get_local`/`emit_set_local` (`:839-855`) already emit plain `GET_LOCAL`/`SET_LOCAL` for it (a
never-reassigned binding is only ever read after its initializing store, so `SET_LOCAL` fires once at
the declaration — correct). No new opcode for the declaring-frame access.

**VM changes (`src/vm/run.rs` `Op::Closure`, `:1559-1597`):** for a `ParentLocal { slot, by_value }`
upvalue:
- `by_value == false` (the existing path): clone the parent frame's cell `Cc` (shared by reference).
- `by_value == true` (new): read the parent frame's **plain slot value** (`fiber.local(slot).clone()`,
  the stack slot at `slot_base + slot`, NOT a cell) and store it directly into the upvalue. To keep the upvalue vector
  representationally uniform (`Vec<Cc<RefCell<Value>>>` — avoiding a `value.rs`/`Closure` shape
  change), wrap the copied value in a FRESH `Cc::new(RefCell::new(v))` owned solely by this closure.
  The win is NOT eliminating the cell type but eliminating the **declaring frame's** cell allocation
  + the per-access `RefCell` indirection in the hot frame: the captured slot is now a plain stack
  local (`GET_LOCAL`, no borrow) in the frame that declares it, and the closure holds a private copy.

  > **Open design choice (owner):** two valid representations for the by-value upvalue — (a) the
  > uniform fresh-cell wrap above (zero `value.rs`/`Closure` change, keeps `GET_UPVALUE` branch-free,
  > but still allocates one `Cc` per closure-capture), or (b) a `Closure.upvalues` that can hold a
  > plain `Value` for by-value captures (a representation change to `value_ext::Closure`, eliminating
  > the cell allocation entirely but adding a branch to `GET_UPVALUE`). **(a) is recommended** because
  > it keeps `value_ext`/`value.rs` untouched and the hot-loop win comes from the *declaring frame*
  > losing its cell, not from the closure-build allocation. (b) is a follow-up if profiling shows the
  > closure-build allocation dominates. SP8 implements (a); the plan notes (b) as deferred.

### Why it is byte-identical

- A `captured && !mutated` binding has the SAME observable value whether shared via a cell or copied:
  it is never reassigned, so there is no later mutation any capturer could observe. The copy is taken
  at `Op::Closure` time, after the binding's initializing store has run (a never-reassigned binding's
  single store is its declaration; the closure expression evaluates after it in source/bytecode
  order). This is the exact invariant the V5 plan and VM spec state.
- **Per-iteration loop freshness is preserved.** Today `FreshCell` (`src/vm/run.rs:1610-1618`,
  emitted at loop-top, `loop_fresh_cells`) refreshes a captured loop-local each iteration so closures
  created in different iterations capture distinct values. For a by-value capture the freshness is
  AUTOMATIC: each iteration's `Op::Closure` copies that iteration's plain slot value, so each closure
  gets its own iteration's value — identical observable behavior, and `FreshCell` is simply not
  emitted for a now-plain slot (only `captured && mutated` slots stay cells and keep `FreshCell`).
- **The mutated-after-capture case keeps the cell.** A binding that IS reassigned (`mutated == true`)
  stays in `cell_slots` and is captured by reference exactly as today — a counter closure
  (`fn make() { let n = 0; return fn() { n = n + 1; return n } }`) still shares the cell so the
  mutation is visible. This is purely additive: the by-reference path is unchanged.
- **KILL SWITCH note:** capture-by-value is a RESOLVER/COMPILER decision (baked into the chunk's
  upvalue descriptors + `cell_slots`), NOT a runtime specialization gated on `self.specialize`. It is
  unconditionally byte-identical across specialized and generic VM (both run the same chunk). The
  three-way differential covers it directly: tree-walker (always by-reference cells via its
  `Environment`) == both VMs (by-value where eligible), because the observable values match.

### Expected win

Closure-heavy code that captures constants (config objects, captured helper functions, captured
loop-invariant values) loses, per such binding: one cell allocation at frame entry, and a `RefCell`
borrow on every read in the declaring frame (now a plain `GET_LOCAL`). The win is measured on a new
closure-capture-heavy bench added to `tests/vm_bench.rs` (e.g. a hot loop that builds a closure
capturing a never-reassigned local each iteration, or a function with several captured-constant
upvalues called in a loop). Recorded in the plan's measurement gate. No regression on existing benches
(the existing ones capture mutated counters → cells → unchanged).

---

## Non-goals (explicitly out of SP8)

- The (b) representation (a `Closure.upvalues` holding plain `Value`s) — deferred follow-up to §2; SP8
  ships (a) (uniform fresh-cell wrap, zero `value.rs` change).
- Per-global generation counters (§1's rejected alternative) — documented fallback only.
- Any tree-walker (`src/interp.rs`) change — it stays the byte-identical oracle, untouched.
- Any `src/value.rs` change — neither optimization touches the value model.
- Promoting a hot top-level `let` from a user-global back to a frame-local (would break the
  late-binding module-env semantics SP-era globals deliberately reproduce) — out of scope; §1 keeps
  globals as globals and only speeds their access.

---

## Testing & quality bar (whole sub-project)

- **Differential oracle never relaxed:** whole-corpus three-way (tree-walker == specialized-VM ==
  generic-VM) byte-identical (`tests/vm_differential.rs`), plus recorded goldens, plus new
  per-feature differential cases (global reassignment loops, shadowing, redeclaration, immutable
  cross-chunk; closures capturing constants, mutated counters, per-iteration loop captures). Any
  divergence on valid code = fix the root cause, never weaken the assertion or edit a tree-walker
  test to match the VM.
- **Both feature configs:** `cargo test` green default AND `--no-default-features`.
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`;
  `await_holding_refcell_ref` stays `deny` + clean. (The new global-access helpers must not hold a
  `user_globals` `RefCell` borrow across an `.await`; all global reads/writes are synchronous, so
  this is naturally satisfied — verify.)
- **Perf gate:** `cargo test --release --test vm_bench -- --ignored --nocapture`. Geomean spec/tw
  back toward ~2.9× (record the achieved number); `≥2×` on every compute-bound bench (NEVER relaxed);
  NO spec-vs-generic regression on ANY bench; the new closure-capture bench shows a measurable win.
- **No grammar/`.aso` change:** SP8 changes neither syntax nor bytecode layout. (The upvalue
  descriptor gains a `by_value` bit — if `FnProto`/`Chunk` upvalue descriptors are serialized in
  `.aso`, bump `ASO_FORMAT_VERSION` and round-trip; verify against `src/vm/aso.rs` whether upvalue
  descriptors are persisted. If they are NOT serialized — protos are recompiled from source on load —
  no bump is needed. The plan's Task B0 checks this first.)
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-phase review (re-read spec, re-run gates, adversarial divergence hunt) before
  sign-off.

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Global cache | `src/vm/adapt.rs` (`GlobalCache::IndexBound`), `src/vm/chunk.rs` (reuse `global_caches`; possibly a parallel set-site cache), `src/vm/run.rs` (`Op::GetGlobal`/`Op::SetGlobal` fast paths, `struct_gen` Cell + bump on define) |
| Capture-by-value | `src/syntax/resolve/{mod,types.rs}` (`cell_slots` = `captured && mutated`; `value_capture_slots`; `UpvalueDescriptor::ParentLocal { by_value }`), `src/compile/mod.rs` (consume the narrowed cell set — likely no change beyond reading the new field), `src/vm/run.rs` (`Op::Closure` by-value upvalue build) |
| `.aso` (conditional) | `src/vm/aso.rs` (only if upvalue descriptors are serialized — bump + round-trip) |
| Tests | `tests/vm_differential.rs` (new global + closure cases), `tests/vm_bench.rs` (re-measure + new closure-capture bench), `src/vm/adapt.rs` unit tests (the `IndexBound` guard) |
