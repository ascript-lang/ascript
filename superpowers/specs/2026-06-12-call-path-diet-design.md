# Call-Path Allocation Diet + Higher-Order Callback Trampoline — Design (CALL)

- **Status:** Implemented (merged on feat/call-path-diet)
- **Date:** 2026-06-12
- **Code:** CALL (Performance & Memory campaign, `goal-perf.md` — Foundation wave, second spec)
- **Depends on:** **LANE** (`superpowers/specs/2026-06-12-two-lane-engine-design.md`) — Unit B
  consumes LANE's synchronous dispatch driver (`Vm::run_loop_sync`, returning
  `SyncOutcome::{Finished(RunOutcome), NeedsAsync}`) and the
  functional-idiom bench corpus LANE's plan Task 0 adds to `bench/`. Unit A has no LANE
  dependency, but the whole spec rebases onto the merged LANE branch (the two touch the same
  dispatch/call files).
- **Compounds with:** **DECODE** (a leaner call path is what gets pre-decoded — no hard edge;
  DECODE depends only on LANE). **Depended on by:** **JIT** (the
  compiled calling convention inherits whatever frame-binding shape this spec ships).
- **Engines:** the VM call path (`src/vm/run.rs`, `src/vm/fiber.rs`) + the stdlib higher-order
  callback sites (`src/stdlib/`). **The tree-walker is UNTOUCHED** — the stdlib builtins are
  shared between engines (they live on `impl Interp`), so every stdlib change here must keep the
  tree-walker path byte-identical; the trampoline is a VM-only fast path that activates only when
  the callee is a VM `Value::Closure` (§5.1).
- **Breaking:** **no.** No syntax change, no semantics change, no `.aso` change
  (`ASO_FORMAT_VERSION` untouched), no opcode change. Runtime-internal allocation shape only.

### Implementation deltas (recorded against this spec, 2026-06-13)

Three narrowings relative to the spec text, each documented and intentional:

1. **Stream-stage trampoline is per-element, not cross-element.** The spec §5.3 sketched a
   per-pipeline-stage trampoline reuse. The shipped implementation is per-element: a single
   `CallbackTrampoline` fiber is taken from the pool at the start of each element and returned
   on `Done`. Cross-element reuse across a `Stage` boundary was deferred because `Stage` must be
   `Clone` but `CallbackTrampoline` is not (it owns a live `Fiber`). Documented as a follow-up
   for DECODE (which may restructure the stage model). Behavior is byte-identical.

2. **`Op::CallMethod` in-place binding deferred to DECODE.** The spec §3 listed in-place binding
   for both `Op::Call` and `Op::CallMethod`. The shipped A2 covers only the `Op::Call`
   plain-Closure arm (the dominant case). `Op::CallMethod` in-place binding is recorded as the
   first DECODE task (§7 follow-up). The method-IC fast path was unchanged. Behavior and alloc
   budget gates are met on the shipped subset.

3. **`smallvec` alternative not needed.** The spec §3.5 recorded a `SmallVec<[Value; 4]>`
   fallback for A2. In-place stack-window binding sufficed to reach 0 allocs/qualifying call —
   the smallvec path was never exercised and is not shipped.

---

## 1. Summary & motivation (the evidence)

Phase-0 profiling (`bench/PROFILING_RESULTS.md`) attributes `object_churn`'s worker-thread time
as **dispatch/VM 49%** — of which `run_loop` 18%, **`Fiber::frame` 9%, frame push/pop 6%** — plus
**allocation 22%**. The call path is where those two slices meet, and code inspection confirms
the constant factors (all citations verified against the tree on 2026-06-12):

1. **≥3 heap allocations per call, even for a capture-free function.**
   - The **cells vector**: `alloc_cells` (`src/vm/fiber.rs:56`) runs `vec![None; slot_count]`
     (`fiber.rs:60`) on EVERY frame — including the overwhelmingly common frame whose
     `chunk.cell_slots` is empty (post capture-by-value resolver work, most functions capture
     nothing by reference). Call sites: `Fiber::new` (`fiber.rs:84`), the `Op::Call`
     plain-closure frame push (`src/vm/run.rs:1790`), and the `Op::CallMethod` IC same-fiber
     push (`run.rs:4961`).
   - A fresh **`Cc<RefCell<Value>>` per captured slot** (`fiber.rs:65`) — necessary when a slot
     IS a cell, pure waste to size the vector for when none is.
   - The **`Vec<Value>` argument collection**: the `Op::Call` plain-closure arm pops the args
     into `vec![Value::Nil; argc]` (`run.rs:1766`) even though they are already contiguous on
     the operand stack directly above the callee (`run.rs:1603-1607`), then `check_call_args`
     builds a SECOND `Vec` (`BoundArgs.values`, `src/interp.rs:7970`) that is copied back into
     the very window the args came from (`run.rs:1796-1802`).

2. **A full fiber + boxed async future per callback ELEMENT in every higher-order builtin.**
   `arr.map(f)` (`src/stdlib/array.rs:52-62`) runs, for EVERY element:
   `f.clone()` + `vec![item]` + `self.call_value(...).await` (`array.rs:58`) →
   `Interp::call_value`'s `Value::Closure` arm (`src/interp.rs:4557`) → `Vm::call_value`
   (`src/vm/run.rs:4462`, itself `#[async_recursion]` — a boxed future per element) →
   `check_call_args` (`run.rs:4534`) → a brand-new `Fiber::new` (`run.rs:4540` — two `Vec`s +
   the cells vector) → `grow_future(self.run(&mut fiber)).await` (`run.rs:4568`). The same
   ceremony repeats in `filter` (`array.rs:69`), `reduce` (`array.rs:82`), the sort comparator
   (`array.rs:152`), `find`/`findIndex`/`some`/`every` (`array.rs:189/204/220/235`),
   `flatMap`/`groupBy`/`partition` (`array.rs:277/384/404`), `object.mapValues`
   (`src/stdlib/object.rs:347`), and the `std/stream` pipeline ops
   (`src/stdlib/stream.rs:577/581/600/669/680/702`). Idiomatic functional code — the exact
   blind spot the profiling corpus had — pays the entire async ceremony per element.

LANE's plan Task 0 adds the functional-idiom benchmarks (map/filter/reduce pipelines,
call-heavy workloads) to `bench/` **before** this spec's work begins, so every change here has a
measured before/after (goal-perf Gate 16). This spec removes the ceremony in two independent
units:

- **Unit A — the allocation diet** (§2–§4): three individually-benchmarked, individually-
  revertible items that remove the per-call allocations on the in-VM call path.
- **Unit B — the callback trampoline** (§5–§7): higher-order builtins drive a plain (non-async,
  non-generator, non-worker) VM closure callback through ONE reused fiber on LANE's sync
  driver — no per-element `Vec`, no boxed future, no fresh `Fiber`, no per-element `f.clone()` —
  with a per-element escalation fallback when the callback genuinely suspends.

Nothing about contract semantics changes: **`check_call_args`' checks still run on every call**
(eliding them via static proof is ELIDE, a different spec); only the allocation ceremony around
the checks is removed.

## 2. Unit A, item A1 — the empty-cells fast path

### 2.1 Design options evaluated

| Option | Shape | Verdict |
|---|---|---|
| (a) `Option<Box<[Option<Cc<RefCell<Value>>>]>>` on `CallFrame` | `None` when no cell slots | Rejected: changes the field type, touches every `cells` consumer's type, and buys nothing over (c) — `Option<Box>` saves 8 bytes of `CallFrame` but adds a discriminant branch at every access. |
| (b) shared static empty slice (`&'static []`) | borrow-typed field | Rejected: turns the owned field into a borrow or a `Cow`, infecting `CallFrame` with a lifetime or an enum; the cell accessors stop being plain `Vec` ops. |
| (c) **an empty `Vec`** — `alloc_cells` returns `Vec::new()` when `cell_slots.is_empty()` | field type unchanged | **Chosen.** `Vec::new()` is allocation-free by construction (no heap touch until first push). Zero type churn; the existing `fiber.rs` cell accessors (`get_local_cell`/`set_local_cell`/`fresh_cell`, `fiber.rs:182-223`) already go through `.get(slot)` so an empty vector is safe there by construction. |

### 2.2 Mechanism

```rust
// src/vm/fiber.rs — alloc_cells
if cell_slots.is_empty() {
    return Vec::new();        // allocation-free; the common case allocates NOTHING
}
let mut cells = vec![None; slot_count];
...
```

**The one real change this forces:** the frame-binding loops index `cells[slot]` directly
(`run.rs:1687/1797`, `run.rs:4517/4547`, `run.rs:4964/4973`, and the
`invoke_compiled_method`/`invoke_compiled_static` binding loops) — indexing an empty `Vec`
panics. Every such site moves to the bounds-tolerant form already used by the accessors:

```rust
if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) { ... } else { ... }
```

That is a length compare + branch — branch-cheap, and on the empty-vector path the compare is
against 0 (predictably not-taken). The cell-access **opcodes** (`Op::GetLocalCell`
`run.rs:2755`, `Op::SetLocalCell` `run.rs:2760`, `Op::FreshCell` `run.rs:2765`) are emitted by
the compiler ONLY for resolver cell slots, so they can never observe an empty vector — their
`expect("non-cell slot (compiler/resolver bug)")` contract is unchanged. Measured per Gate 18
(allocation-count slope test + bench, §8).

`cells.clone()` snapshot sites (`run.rs:1685/4515/4545`) clone an empty `Vec` for free —
no change needed, they just stop costing.

## 3. Unit A, item A2 — in-place argument binding over the operand-stack window

### 3.1 Today vs the classic convention

At `Op::Call` the stack is `[..., callee, arg0, .., arg{argc-1}]` and the new frame's
`slot_base` is `callee_idx` — i.e. **the args already sit one slot above where they must end
up** (`run.rs:1603-1607`, `run.rs:1785`). Today the VM pops them into a `Vec`, runs
`check_call_args` (which builds `BoundArgs.values`, a second `Vec`), and writes the values back
into the window. The classic stack-VM convention binds the frame window directly over the
pushed args; the only reason we can't always do that is that `check_call_args` also performs
default-placeholder layout and rest-tail collection.

### 3.2 Which call shapes qualify (exact, conservative)

A call binds **in place** when ALL hold:

1. The callee is a plain `Value::Closure` taking the in-fiber frame-push path
   (`!is_async && !is_generator && !is_worker` — the existing `run.rs:1757` arm; the
   generator/async/worker arms are untouched).
2. **`!proto.has_rest`** — a rest param genuinely materializes a new tail array
   (`interp.rs:8000-8031`); those calls keep today's path verbatim.
3. The arity check passes with `argc <= n_positional` (always true once 2 holds and the
   too-many check passes). **Defaults do NOT disqualify**: `check_call_args` never evaluates
   defaults — it lays `Value::Nil` placeholders (`interp.rs:7993-7998`) and the callee
   PROLOGUE (`Op::JumpIfArgSupplied`, `run.rs:2225`, reading `frame.argc`) evaluates them in
   the callee frame. In-place binding produces the identical layout: the supplied args occupy
   slots `0..argc`, `stack.resize(slot_base + slot_count, Value::Nil)` fills every remaining
   slot with the same `Nil` the placeholders carried, and `frame.argc = argc` drives the same
   prologue.
4. The `call_fast` kill switch is on (§8.1).

Everything else — rest-param callees, `CallNamed`, non-closure callees — falls back to today's
pop-into-`Vec` path unchanged.

### 3.3 Mechanism

A borrowing twin of the arg checker is extracted so the checks themselves stay in ONE place
(byte-identical messages by construction, not by parallel maintenance): `check_call_args`
(`interp.rs:7898`) is refactored into shared cores — the arity check (the exact
`expected N` / `at least` / `at most` wording, `interp.rs:7928-7968`) and the per-param
contract check (the env-aware `check_type_env` / `check_type` fallback + `contract_panic`,
`interp.rs:7977-7990`) — consumed by both the existing `Vec`-consuming `check_call_args`
(every other caller keeps its exact signature and behavior) and a new
`check_call_args_in_place(params, args: &[Value], span, what, interp, env)` that runs the
IDENTICAL checks left-to-right over the stack window without consuming or producing a `Vec`.
Both contract checks are synchronous (`check_type_env` is `?`-able but never awaits), so no
borrow crosses an await.

The qualifying `Op::Call` arm then:

1. checks contracts in place over `&fiber.stack[callee_idx + 1 ..]`;
2. `fiber.stack.remove(callee_idx)` — drops the callee value, shifting exactly the `argc`
   args down one slot so they start AT `slot_base` (an `argc`-element memmove, no allocation);
3. `resize(slot_base + slot_count, Value::Nil)`; if `cells` is non-empty, moves any param
   value whose slot the resolver promoted to a cell out of the window into its cell
   (`mem::replace` with `Nil` — rare; only a callback that mutates a captured param);
4. `enter_frame_depth` + `frames.push` exactly as today (`run.rs:1807-1820`), `argc` = the
   supplied count.

**Zero allocations** for the qualifying shape (when A1 also makes `cells` empty).

### 3.4 Panic-flow byte-identity (the one observable-shape note)

On an arity/contract panic, today's path has already popped args+callee off the stack; the
in-place path panics with them still on it. This is **unobservable**: a `Control::Panic`
raised during `Op::Call` binding unwinds out of `run_loop` via Rust `?` and the fiber is never
resumed (there is no in-fiber catch; `recover` drives a separate fresh fiber via `call_value`,
and a generator whose `resume` returns `Err` is Done). The message, span, and check ORDER are
byte-identical because they are the same code. The four-mode differential + the fuzzer axis
(§8) are the proof, and a dedicated panic-wording parity battery pins the messages.

## 4. Unit A, item A3 — fiber & cells pooling for re-entrant calls

### 4.1 The cost

Every native→VM re-entry — `Vm::call_value` (`run.rs:4540`), `invoke_compiled_method`
(`run.rs:5604`), `invoke_compiled_static` (`run.rs:5693`), `vm_construct` — builds a brand-new
`Fiber` (two `Vec`s: `frames`, `stack`) and drops it on completion. Unit B removes the
per-element case entirely for trampolined callbacks, but every remaining re-entry (method
dispatch through `call_value`, `recover`, schema refiners, non-trampolined callbacks, the
escalation path itself) still pays it.

### 4.2 Lifecycle (specified precisely — a pooled fiber must never be live twice)

- `Vm.fiber_pool: RefCell<Vec<Fiber>>`, capped at `FIBER_POOL_MAX = 8` (bounds memory under
  pathological re-entrancy; beyond the cap, fibers are simply dropped as today).
- **Take = exclusive ownership transfer.** `take_pooled_fiber(top)` pops a fiber from the pool
  (or builds `Fiber::new(top)` when empty/disabled) and `reset`s it: `frames` cleared and one
  fresh frame pushed at `ip 0`, `stack` cleared then resized to `slot_count` `Nil`s, fresh
  `alloc_cells` (cells are NEVER reused across calls — capture identity), state `Running`.
  Because take REMOVES the fiber from the pool, recursion and re-entrancy are safe by
  construction: a nested `call_value` while an outer one is live simply takes a different
  fiber (or allocates). The pool is a `RefCell` but every borrow is a short synchronous
  `pop`/`push` — **never held across an await**.
- **Return only on clean completion.** The driving site returns the fiber to the pool after
  `RunOutcome::Done`. On `Err` (panic/propagate unwound out of `run`) the fiber is **dropped,
  never pooled** — its frames/stack are mid-flight state; dropping is today's behavior and
  poisoning is structurally impossible.
- **Never pooled:** generator fibers (`GeneratorHandle::new_vm` owns its fiber for the
  generator's life, `run.rs:1693/4523`), the module fiber (`run.rs:987`), and the program's
  root fiber. Only the transient run-to-completion re-entry fibers participate.
- A pooled fiber's drive may cross awaits (the closure body can suspend) — that is fine: the
  fiber is exclusively owned by that `call_value` invocation until Done, exactly as an owned
  fresh fiber would be. The pool only ever holds fibers with `frames.is_empty()`.

Per-call **cells** pooling for protos WITH cell slots was evaluated and **rejected for v1**: a
cell's identity is observable (closures capture the `Cc`), so cells can never be reused — only
the outer vector allocation could be pooled, which A1 already makes free for the common case
and which is a `Vec<Option<_>>` of typically 1–3 entries otherwise. Not worth the lifecycle
risk; recorded here so it isn't re-litigated.

Each diet item (A1, A2, A3) lands as its own commit behind the same kill switch, is
**benchmarked individually** (allocation-count slope + time, §8.4), and is individually
revertible.

## 5. Unit B — the higher-order callback trampoline (the headline)

### 5.1 The engine seam (read from the code, not assumed)

The higher-order builtins live on **`impl Interp`** (`src/stdlib/array.rs:43-44`) and are
shared by both engines. Their per-element call goes through `Interp::call_value`
(`src/interp.rs:4542`), which dispatches **by callee kind**: a tree-walker callback is a
`Value::Function` → `call_function` (the tree-walker path, untouched); a VM-compiled callback
is a `Value::Closure` — a kind **only the VM can produce** — and routes to the registered VM
via the `Interp.vm` weak (`interp.rs:486`, `interp.rs:4557-4561`). **That is the seam:** the
trampoline arms only for a `Value::Closure` callee with a live registered `Vm`, so it is
VM-only *by construction* — on a pure tree-walker run no closure exists, no trampoline ever
arms, and the tree-walker path is bit-for-bit today's code.

### 5.2 Eligibility (per callee, checked once per builtin loop)

`CallbackTrampoline::arm(interp, f, span) -> Option<CallbackTrampoline>` returns `Some` iff:

- `f` is `Value::Closure(c)` with `!c.proto.is_async && !c.proto.is_generator &&
  !c.proto.is_worker` (`FnProto` flags, `src/vm/chunk.rs:492`). Async callees must keep the
  eager-spawn `Value::Future` semantics; generator callees must return a `Value::Generator`;
  a pooled `worker fn` value must dispatch to its isolate (`run.rs:4491-4498`) — all three
  fall to the generic path untouched.
- `interp.vm()` upgrades (always true when a closure exists; checked anyway — a `None` is the
  generic path, never a panic) and `vm.call_fast` is on (§8.1).

`arm` clones the closure's `Cc` **once** (replacing the per-element `f.clone()`); each builtin
site keeps a single loop over a small `CallbackDriver` enum — `Tramp(CallbackTrampoline)` or
`Generic { f, span }` (the exact today path) — so no site forks its logic.

### 5.3 The per-element fast path

For each element, `CallbackTrampoline::call(&mut self, args: &mut [Value])` (with `call1`/
`call2` conveniences for the 1- and 2-arg shapes — the args live in a stack array at the call
site; **no per-element `Vec`**):

1. **`check_call_args` semantics still run per element** via the same borrowing core A2
   introduced (`check_call_args_in_place` over the args slice) — identical arity/contract
   panics, identical left-to-right order, anchored at the same builtin `span` as today.
2. The ONE reused fiber is `reset` (the invariant below), `ret_span`/`argc` set, and the args
   `mem::replace`d directly into the frame's slot window (cell-aware, like every binding loop).
3. **`call_depth` exactly once per logical call (SP3):** `enter_call_depth_scoped(span)` RAII
   per element — the same single increment today's `call_value` does (`run.rs:4559`). A
   recursion-depth program trips at the identical depth in all modes.
4. Drive the fiber on **LANE's sync driver**: `crate::vm::stack::grow(|| vm.run_loop_sync(&mut
   fiber))` — a plain non-async call, **no boxed future, no `.await`** on the hot path.
5. **Escalation fallback:** if the sync driver returns `NeedsAsync` (the callback transitively
   hit a suspension op — it called an `async fn` and awaited, awaited a pending future arg,
   resumed a generator, hit a patched `Op::Break`, or any other LANE escalation trigger), the
   trampoline **continues the SAME live fiber on the async driver**:
   `grow_future(vm.run(&mut fiber)).await` — it must **never re-dispatch the element through
   `call_value`** (the element has already executed side effects up to the suspension point;
   re-running would double them). This costs that element today's boxed-future price and
   nothing more. The NEXT element re-arms the sync path — per-element re-arming keeps the fast
   path hot while staying correct.
6. On `Finished(RunOutcome::Done(v))`, the fiber is retained for the next element; the result
   is returned. `Err`
   (`Control::Panic` / `Control::Propagate`) propagates out exactly as today — the builtin's
   `?` carries it to the caller, so `recover`, `?`-propagation out of the builtin, and panic
   abort flows are byte-identical (the `Control` value is the same object the inner `run`
   produced; nothing is wrapped or rewritten).

### 5.4 The reset invariant (specified precisely, unit-tested)

Before EVERY element — regardless of how the previous element ended (Done, Panic, Propagate,
escalated-then-Done) — `Fiber::reset(closure)` re-establishes exactly:

- `frames.len() == 1`; the frame is `{ closure, ip: 0, slot_base: 0, cells:
  alloc_cells(slot_count, cell_slots), ret_span/def_class/argc set by the caller }`;
- `stack.len() == slot_count`, every slot `Value::Nil` (then params bound over `0..argc`);
- `state == FiberState::Running`.

`reset` clears `frames` and `stack` **unconditionally** (a panicking element can leave
arbitrary mid-flight frames; `return_from_frame` on clean completion already truncated to
empty — `run.rs:5813` — but reset does not rely on that). `cells` are freshly allocated per
element: a closure the callback created in element N that captured a cell keeps THAT cell;
element N+1 must get a fresh one (capture freshness — same reason `Op::FreshCell` exists).
With A1, this fresh allocation is free for the common capture-free callback. A dedicated unit
battery proves: a panic mid-element does not poison the next element; per-element captured
cells are distinct; return-type contracts check against the same `ret_span`.

### 5.5 Wired sites (enumerated; per-site decision)

**Wired in v1 — the per-element ITERATION sites** (a loop amortizes the one-time arming):

| Site | File:line (callback call) |
|---|---|
| `array.map` / `filter` / `reduce` | `src/stdlib/array.rs:58 / 69 / 82` |
| `array.sort` comparator | `array.rs:152` |
| `array.find` / `findIndex` / `some` / `every` | `array.rs:189 / 204 / 220 / 235` |
| `array.flatMap` / `groupBy` / `partition` | `array.rs:277 / 384 / 404` |
| `object.mapValues` | `src/stdlib/object.rs:347` |
| `stream` pipeline stages (`map`/`filter`/`takeWhile`) + `each`/`reduce`/`find` | `src/stdlib/stream.rs:577 / 581 / 600 / 669 / 680 / 702` |

**Deliberately NOT wired (documented decision, not a deferral):** every remaining
`call_value` site is a single-shot or per-event invocation with no element loop to amortize
over — `events.rs:165` (listener dispatch), `sync.rs:576`, `task_mod.rs:92/275` (spawn/retry),
`time_timers.rs:255/324`, `bench.rs:69`, `assert_mod.rs:634/680`, `schema.rs:935` (refiner),
`net_http.rs:1319`, `workflow.rs:489/642` (replay determinism — leave the seam alone),
`http_server.rs:437/2053/2077` (handler/middleware — typically async anyway). They keep the
generic path and STILL benefit from Unit A's pooling (A3) and empty-cells (A1). If a future
profile shows one of them hot, wiring it is a one-line `CallbackDriver` adoption.

### 5.6 What stays byte-identical (the checklist)

- `check_call_args` arity/contract checks per element — same code, same order, same wording.
- `call_depth` exactly once per logical call; recursion-limit parity.
- Panic/Propagate `Control` flow out of the builtin — same values, untouched.
- An async/generator/worker callee — never arms; identical behavior.
- The tree-walker — `Value::Function` callbacks never reach the trampoline.
- Scheduling: today's per-element `call_value(..).await` does not yield to the reactor unless
  the body suspends (an immediately-ready future polls inline), so removing the await for the
  non-suspending case does not change observable task interleaving; a suspending callback
  escalates onto exactly today's async drive.

## 6. Honest performance expectations (measured, never promised)

- **Functional-idiom corpus (the headline):** the removed per-element ceremony is a boxed
  `async_recursion` future + a fresh `Fiber` (2 Vecs) + a cells vector + 2 arg `Vec`s + a
  `Value` clone, replaced by an in-place bind + a sync dispatch loop entry. Expected: a large
  integer factor on map/filter/reduce pipelines. **The number comes from the A/B harness
  (`bench/CALL_RESULTS.md`), never from this spec.**
- **Call-heavy non-callback code:** A1+A2 remove 2–3 allocations per call on the in-VM path;
  expected measurable improvement on `object_churn`-class workloads (alloc was 22%); again,
  measured.
- **Zero effect on non-callback, non-call-heavy code** — and PROVEN zero: the kill-switch-off
  configuration must measure at parity, and `dbg_zero_cost_gate` is re-run (the call path is
  touched, Gate 17).
- **Memory (Gate 18):** allocation counts measured before/after via the slope method (§8.4);
  peak RSS reported on the corpus via `/usr/bin/time -l`. The fiber pool adds at most
  `FIBER_POOL_MAX` retained small fibers per isolate — bounded by design, reported.

## 7. Scope & non-goals

- **No semantics change anywhere.** Any observable divergence is a bug in this spec's code.
- **ELIDE is out of scope:** `check_call_args` runs on every call, per element. Skipping
  proven-safe checks is the separate ELIDE spec.
- **Rewriting `call_value`'s public signature project-wide — rejected for v1.** The
  `Vec<Value>` stays at the generic boundary (`Interp::call_value` / `Vm::call_value` keep
  their signatures; ~30 stdlib callers untouched); only the hot paths bypass it. A
  project-wide `&mut [Value]` calling convention is a DECODE/JIT-era consideration.
- **`smallvec` for args — evaluate-and-measure alternative, not v1.** If A2's in-place binding
  proves too narrow in practice (the no-rest restriction excluding a measured-hot shape), a
  `SmallVec<[Value; 4]>` at the `Op::Call` pop is the recorded fallback; it would be adopted
  only on a measured win, and the decision recorded in `bench/CALL_RESULTS.md`.
- **`Op::CallMethod` in-place binding — recorded follow-up, not v1.** The method-IC fast path
  (`run.rs:4946-5003`) receives its args already collected by `dispatch_method`; rebinding
  that path in place requires restructuring the shared method dispatch and is deferred to
  DECODE (which rebuilds that decode path anyway). It DOES get A1 (empty cells) and its
  `call_value`-routed cases get A3 (pooling).
- **Cells pooling — rejected** (§4.2): cell identity is observable; only the outer vector
  could pool, which A1 already makes free in the common case.

## 8. Correctness — kill switch, differential modes, fuzz axis, coverage assertion

### 8.1 ONE kill switch, folded into the `--no-specialize` discipline (decision + justification)

A single permanent flag `Vm.call_fast: bool` (default `true`) gates **every** fast path this
spec adds: A1's empty-cells return, A2's in-place binding, A3's pooling, and B's trampoline
arming. Decision: **follow the existing `--no-specialize` discipline exactly** rather than
adding a CLI flag — there is no CLI `--no-specialize` today either (the kill switch is the
engine-level `Vm::new_generic` / `with_specialize(false)` + `#[doc(hidden)]` test entry
points, `src/lib.rs:2238-2242`); CALL adds surface the same way:

- `Vm::new_generic` (the everything-off floor) sets **both** `specialize = false` AND
  `call_fast = false` — **the generic path remains complete** (no fast path of any kind), so
  the existing generic differential mode transitively covers CALL-off.
- A new isolating configuration `vm_run_source_no_call_fast` (`specialize = true, call_fast =
  false`) joins `vm_run_source_cfg` (`src/lib.rs:2269`) so a CALL-path divergence is
  distinguishable from an IC/adaptive divergence.
- **Env seam:** the kill switch is also reachable without a code change via
  `ASCRIPT_NO_CALL_FAST=1`, read once at `Vm` construction (the LANE `ASCRIPT_NO_SYNC_LANE`
  precedent, itself mirroring `ASCRIPT_NO_SPECIALIZE`, `src/lib.rs:2067`) — so worker isolates
  inherit it automatically; tests use the explicit constructor/setter, never the env
  (parallel-test hygiene). Campaign convention, restated: kill switches are `ASCRIPT_NO_*` env
  vars; value-selectors (thresholds, counts) are value-style env vars (`ASCRIPT_WORKERS`-shaped).
- The flag is permanent (not bring-up scaffolding), mirroring `specialize` (`run.rs:105-117`).

### 8.2 Differential modes (same PR, Gate 15)

`tests/vm_differential.rs` asserts, over the whole corpus + goldens in BOTH feature configs:

> tree-walker == specialized-VM == generic-VM == **no-call-fast-VM**

plus the targeted parity batteries: arg-panic wording (arity/contract/rest/defaults), the
recursion-limit depth, trampoline panic/propagate/recover flows, escalating callbacks, and
per-element capture freshness. Fix the engine, never the assertion.

### 8.3 Fuzz axis + coverage assertion (anti-false-green)

- `fuzz/fuzz_targets/differential.rs` gains the fourth engine run
  (`vm_run_source_no_call_fast`) in the same oracle assertion, and the in-tree
  `tests/property.rs` three-way differential becomes four-way — same PR.
- **Coverage assertion:** cheap always-on counters on `Vm`
  (`call_fast_stats: Cell<CallFastStats>` — `trampoline_calls`, `trampoline_escalations`,
  `inplace_binds`, `pooled_fiber_reuses`; plain `Cell` bumps, cost verified by the Gate-17
  bench) exposed via a `#[doc(hidden)]` accessor. A corpus test runs the functional-idiom
  examples with `call_fast` on and **asserts `trampoline_calls > 0` and `inplace_binds > 0`**
  (and, on the escalation example, `trampoline_escalations > 0`) — the differential must prove
  the new paths actually ran, not merely that nothing crashed (the JIT spec's anti-false-green
  rule, applied here).

### 8.4 Memory gate (Gate 18 — the measurement method, documented)

- **Allocation counts:** a dedicated integration-test binary `tests/alloc_count.rs` installs a
  counting `#[global_allocator]` (wrapping `System`; a relaxed atomic per `alloc`). Each probe
  runs the same program at N and 2N callback elements and takes the **slope**
  `(allocs(2N) − allocs(N)) / N` = allocations per element — immune to runtime/startup noise.
  Asserted: trampoline-on slope is below an explicit budget AND below half the
  trampoline-off slope (both runs in the same binary via the two entry points). The same
  method probes A1 (capture-free call slope) and A3 (re-entrant call slope) individually.
- **Peak RSS:** `bench/run_call_bench.sh` records `/usr/bin/time -l` peak RSS for the corpus,
  before/after, into `bench/CALL_RESULTS.md`. A memory regression is a bug, never a tradeoff.

### 8.5 Standing gates

`goal.md` Gates 1–14 and `goal-perf.md` Gates 15–18 apply in full (the plan's final task lists
them): four-mode byte-identity both configs, clippy both configs, full suite both configs, no
borrow across await (the pool/trampoline borrows are synchronous), zero `type-*` corpus FPs,
spec/tw geomean ≥2× holds, **`dbg_zero_cost_gate` re-run** (the call path is touched), docs/
examples/CLAUDE.md/roadmap updated, and any bug found en route fixed in-branch
failing-test-first.

## 9. Grounding (verified citations, 2026-06-12)

- `src/vm/fiber.rs:31` (`CallFrame.cells`), `:56-68` (`alloc_cells`; `vec![None; slot_count]`
  at `:60`), `:81-99` (`Fiber::new`; `alloc_cells` call at `:84`), `:182-223` (cell accessors,
  already `.get`-based).
- `src/vm/run.rs:64` (`Vm`), `:105-117` (`specialize`, the kill-switch precedent),
  `:218-231` (`new`/`new_generic`/`with_specialize`), `:616` (`enter_frame_depth`),
  `:987` (module fiber), `:1570-1847` (`Op::Call`/`CallSpread`: `callee_idx` `:1607`,
  plain-closure arm `:1757` — args `Vec` `:1766`, `check_call_args` `:1775`, `alloc_cells`
  `:1790`, `resize` `:1794`, depth+push `:1807-1820`; native arm → `call_value` `:1828-1845`),
  `:2225` (`Op::JumpIfArgSupplied`), `:2240` (`Op::CheckParam`), `:2755/2760/2765`
  (`GetLocalCell`/`SetLocalCell`/`FreshCell`), `:4462-4600` (`Vm::call_value`:
  `check_call_args` `:4534`, `Fiber::new` `:4540`, `enter_call_depth_scoped` `:4559`,
  `grow_future(self.run(..))` `:4568`), `:4946-5003` (`Op::CallMethod` IC same-fiber push;
  `alloc_cells` `:4961`), `:5604` (`invoke_compiled_method`), `:5693`
  (`invoke_compiled_static`), `:5795-5829` (`return_from_frame`; truncate `:5813`).
- `src/interp.rs:486` (the `Interp.vm` weak — the engine seam), `:495-506` (`call_depth`, the
  SP3 exactly-once contract), `:4542-4601` (`Interp::call_value`; the `Value::Closure` arm
  `:4557`), `:7892-7896` (`BoundArgs`), `:7898-8038` (`check_call_args` — arity wording
  `:7928-7968`, contract walk `:7977-7990`, default placeholders `:7993-7998`, rest collection
  `:8000-8031`).
- `src/stdlib/array.rs:43-44` (`impl Interp` — the builtins are engine-shared), callback sites
  `:58/69/82/152/189/204/220/235/277/384/404`; `src/stdlib/object.rs:347`;
  `src/stdlib/stream.rs:577/581/600/669/680/702`; non-wired sites listed in §5.5.
- `src/vm/chunk.rs:407` (`Chunk.cell_slots`), `:492-510` (`FnProto`:
  `arity`/`has_rest`/`is_async`/`is_generator`/`is_worker`/`params`/`ret`).
- `src/vm/stack.rs` (`grow`/`grow_future`, the SP9 re-entry growth discipline the trampoline
  and escalation reuse).
- `src/lib.rs:791/804` (`vm_run_source`/`vm_run_source_generic`), `:2240`
  (`vm_run_source_with` — "the eventual CLI's `--no-specialize` maps to specialize=false"),
  `:2269` (`vm_run_source_cfg`, the config funnel the new mode joins).
- `tests/vm_bench.rs:162` (`Engine`), `:216` (`measure`), `:499` (`dbg_zero_cost_gate`).
- `fuzz/fuzz_targets/differential.rs` (the three-way oracle the fourth axis joins).
- `bench/PROFILING_RESULTS.md` (object_churn: dispatch 49% — `run_loop` 18%, `Fiber::frame`
  9%, push/pop 6%; alloc 22%).
- `goal-perf.md` (the CALL entry, Gates 15–18, the measurement mandate);
  `superpowers/specs/2026-06-12-two-lane-engine-design.md` (LANE — `run_loop_sync`, the
  escalation triggers, the functional bench corpus; CALL consumes
  `Vm::run_loop_sync(&mut Fiber) -> Result<SyncOutcome, Control>` with
  `SyncOutcome::{Finished(RunOutcome), NeedsAsync}` — LANE's exported names, adopted here).
