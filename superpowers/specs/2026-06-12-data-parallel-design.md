# AScript Data-Parallel Primitives over the Frozen Shared Heap — Design (PAR)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** PAR (PERF campaign — see `goal-perf.md`)
- **Depends on:** the shipped worker subsystem (`src/worker/` — Workers Spec A/B, merged) and
  the shipped SRV shared heap (`std/shared`, `Value::Shared(Arc<SharedNode>)`, the `TAG_SHARED`
  airlock side-vector — merged). LANE Task 0's bench-corpus extension (the same-session A/B
  harness discipline); PAR is otherwise independent of every other PERF spec and can run
  alongside any wave (`goal-perf.md` execution order).
- **Depended on by:** nothing.
- **Engines:** BOTH, byte-identical. PAR is **stdlib only** — `task.pmap`/`task.preduce` are
  ordinary `std/task` functions on the engine-shared `Interp` (`src/stdlib/task_mod.rs`), and
  the chunk execution rides the existing worker machinery that is already proven engine-shared
  (`build_code_slice_from_source` is "the SINGLE slice path shared by BOTH engines",
  `src/worker/dispatch.rs:815-829`; the workers corpus examples run four-mode byte-identical
  today, e.g. `examples/workers_parallel_map.as`).
- **Breaking:** no. No syntax, no opcodes, no `.aso` change (`ASO_FORMAT_VERSION` stays at its
  current value — **27** at writing, `src/vm/aso.rs:167`; read the constant, never hardcode),
  no new worker-wire tag (§3.4), no grammar/tree-sitter/formatter change.

---

## 0. Read this first — what PAR is and is not

AScript already has the two halves of Rayon-class data parallelism, shipped and measured:

1. **Shared-nothing workers** (`src/worker/`): a lazy, demand-grown pool of full `Interp`
   isolates bounded to `num_cpus` (`$ASCRIPT_WORKERS`, `src/worker/pool.rs:59-64`), a
   structured-clone airlock (`src/worker/serialize.rs`), code shipping by transitive
   dependency closure (`src/worker/dispatch.rs`), cancel-on-drop, and a pool-side
   code/archive cache that ships each slice **at most once per isolate**
   (`Pool::send_to`, `src/worker/pool.rs:132-164`). Measured: **4.98× on 8 workers** for
   CPU-bound chunks; warm per-call round-trip **~0.23 ms**; arg serialization
   **~1.29 ms / 10k floats** (`bench/WORKERS_RESULTS.md` §1-3).
2. **The frozen shared heap** (SRV): `shared.freeze(v)` builds an immutable, acyclic,
   `Send` `Arc<SharedNode>` DAG that crosses the airlock as **one `Arc` bump** via the
   `TAG_SHARED` side-vector (`src/worker/serialize.rs:107`, `WorkerRequest.shared`,
   `src/worker/isolate.rs:104-108`). Measured: hand-off cost **flat ~0.15 ms regardless of
   table size** vs a linearly-growing deep clone (**31× at 50k entries**); freeze itself is
   **~0.52 ms / 10k entries**, paid once (`bench/SHARED_HEAP_RESULTS.md` §1).

What is missing is the **user-facing primitive that composes them**: today a user hand-rolls
`task.gather(array.map(inputs, workerFn))` (the `examples/workers_parallel_map.as` pattern),
which dispatches **one pool round-trip per element** — at ~0.23 ms warm per call, per-element
dispatch swamps small per-element work — and deep-copies any shared lookup data per call.
PAR adds exactly two stdlib functions:

```
task.pmap(data, f, opts?)        -> future<array>   // parallel map, input order
task.preduce(data, f, init, opts?) -> future<T>     // parallel reduce (f associative)
```

They **chunk** the input across the existing pool, run the callback inside isolates one chunk
per dispatch (amortizing the round-trip over the chunk), and merge results **in input order**.
A frozen input array crosses by `Arc` bump; everything else rides today's airlock semantics.

PAR is deliberately **not** an engine change: no new opcode, no dispatch-loop edit, no fiber
work, no scheduler. It is the ×cores lever built from shipped, measured pieces — which is why
it is independent of LANE/CALL/SHAPE and can merge in any wave. The correctness bar is the
usual one: byte-identical across all four modes, with determinism **designed in** (input-order
merge, contractual chunk boundaries, first-by-input-order error) rather than hoped for.

## 1. Summary & motivation (evidence)

The PERF campaign's multi-core story (`goal-perf.md` §"Multi-core") is: the isolate model and
the frozen heap are shipped; PAR spends them. The honest economics, from the shipped bench
reports:

| Cost | Measured | Consequence for PAR |
|---|---|---|
| Pool warm per-dispatch round-trip | ~0.23 ms (`WORKERS_RESULTS.md` §2) | dispatch per **chunk**, not per element; default chunk count ≈ pool size |
| Pool cold first dispatch | ~83 ms incl. pool spin-up (`WORKERS_RESULTS.md` §3) | first `pmap` in a process pays warmup; documented, amortized |
| Deep-clone of args | ~1.29 ms / 10k floats (`WORKERS_RESULTS.md` §2) | unfrozen input pays one total copy split across chunks (§3.1) |
| Frozen hand-off | flat ~0.15 ms, size-independent (`SHARED_HEAP_RESULTS.md` §1) | frozen input crosses per chunk for ~free — the happy path |
| Freeze walk | ~0.52 ms / 10k entries, once (`SHARED_HEAP_RESULTS.md` §1) | freezing pays off after ~1 dispatch of a 10k table |
| CPU-bound scaling | 4.98× @ 8 workers (`WORKERS_RESULTS.md` §1) | the scaling expectation `pmap` inherits |

Expectation (stated, not promised — Gate 16 measures it): for coarse CPU-bound per-element
work, `pmap` should match the hand-rolled chunked-gather scaling (≈3× on 4 cores, ≈5× on 8 on
the bench host) while being one line; for tiny per-element work the fixed per-chunk dispatch
cost dominates and `pmap` is **slower than a sequential loop** below a measured break-even
(the bench report must publish that break-even, §6).

## 2. Surface & semantics

### 2.1 The API (v1 = exactly two functions)

```javascript
import * as task from "std/task"
import * as shared from "std/shared"

worker fn score(row) { return row.weight * row.hits }      // the callback: a NAMED worker fn
worker fn add(a, b) { return a + b }                       // associative combiner

let rows  = shared.freeze(load_rows())                     // happy path: frozen input
let out   = await task.pmap(rows, score)                   // future<array>, INPUT order
let total = await task.preduce(out, add, 0)                // future<T>

// opts (both functions): { chunks?: int >= 1, minChunk?: int >= 1 }
let out2 = await task.pmap(rows, score, { chunks: 4 })
```

- **`task.pmap(data, f, opts?) -> future<array>`** — applies `f` to every element of `data`
  in pool isolates, returns the results **in input order** (never completion order).
- **`task.preduce(data, f, init, opts?) -> future<T>`** — parallel reduction. Each chunk is
  folded with `f` **seeded by the chunk's own first element** (no `init` inside a chunk);
  the partials are then combined by one final fold `f(...f(f(init, p0), p1)...)` (§3.3.2).
  `init` therefore participates **exactly once** — there is **no identity-element
  requirement** on `init`; the requirement is on **`f`: it must be associative** for
  `preduce` to equal sequential `reduce` (§3.8 states the contract verbatim for the docs).
- Both return a **`future<…>`** (eagerly scheduled, like every worker call — Spec A §3), so
  they compose unchanged with `await`, `task.timeout`, `task.race`, `task.spawn`, and
  structured cancel-on-drop (§3.5).
- **Snapshot-at-call:** the input is captured synchronously inside the call (the frozen `Arc`
  is captured; an unfrozen array's elements are snapshotted before the future is returned),
  so mutating `data` after calling `pmap` never affects the result.
- **Empty input:** `pmap([])` resolves to `[]`; `preduce([], f, init)` resolves to `init` —
  in both cases **without touching the pool** (no lazy-pool initialization; the
  zero-worker-program guarantee of Spec A §7 extends to empty parallel calls).

**v1 set is `pmap` + `preduce` only.** `pfilter` and `peach` are rejected for v1 (§8): a
parallel filter is `pmap` to `[keep, value]` plus a local pass (same cost class, trivially
composable, and it keeps the merge contract single-shaped); a parallel `each` is an
attractive nuisance across isolates — side effects inside an isolate **cannot** touch the
caller's heap, so `peach` would advertise an effect it structurally cannot deliver. Making
users write `pmap` and discard keeps the shared-nothing model visible.

### 2.2 The callback contract (same rules as `worker fn`, by construction)

`f` must be a **named, top-level `worker fn`** — the exact rule `run_in_worker` already
enforces (`worker_fn_dispatch_name`, `src/interp.rs:7753`: a `Value::Function`/`Value::Closure`
whose `is_worker` flag is set and whose name is known). Anything else — an arrow, an anonymous
fn, a plain (non-`worker`) fn, a builtin, a bound method — is a recoverable Tier-2 panic:

```
task.pmap expects a named `worker fn` as its callback (got function)
```

Why this rule and not "any callable": shipping the callback reuses the worker code-shipping
closure **verbatim** (`build_code_slice_for_interp`, `src/worker/dispatch.rs:919` →
`build_code_slice`, `:116` — the GET_GLOBAL fixpoint over top-level fns / consts / classes /
enums / interfaces / imports, with the `.aso`-mode fallback via `resolve_worker_top_chunk`,
`:874`). That machinery ships **top-level** entries; and only `worker fn` bodies are guarded
by the compile-time `worker-capture` checker rule (Spec A §4: params + consts + top-level fns
only; capturing a mutable outer `let` is a checker **Error**). Requiring `worker fn` means
every capture violation is caught statically with the existing diagnostic, and every
sendability violation in the data is the existing recoverable field-path panic — PAR invents
**zero** new capture/sendability rules and zero new diagnostics for them. A `static worker fn`
callback is **not** accepted in v1 (the `for_interp` slice builder resolves free top-level fns;
the static-method variant exists but `run_in_worker` does not route it either) — it panics
with the clear message above; recorded as a follow-up, not a silent gap.

What users can capture in `f` (document verbatim in the docs page): its parameters; top-level
`fn`s/`worker fn`s; `const`s (copied — literal by value, computed by re-running the
initializer on the isolate, `dispatch.rs:30-43`); top-level classes/enums/interfaces
(shipped transitively); imported `std/*` modules. What they cannot: mutable outer `let`s
(checker error), and any non-sendable value inside `data`/`init`/returned values (recoverable
field-path panic, same message as today's worker args).

## 3. Mechanism

### 3.1 Input: frozen = zero-copy; unfrozen = per-chunk airlock copy (NOT auto-freeze)

Two accepted input forms, one panic:

1. **`Value::Shared` whose node is an array** (the happy path): the single `Arc<SharedNode>`
   is shipped to every chunk via the existing `TAG_SHARED` side-vector — the SRV **path-b**
   mechanism: `serialize::encode` pushes the `Arc` into the `Writer.shared` vector and writes
   a `TAG_SHARED` index (`serialize.rs:107`, `:423`); the `Arc`s ride
   `WorkerRequest.shared: Vec<Arc<SharedNode>>` (`isolate.rs:104-108`) and reconstruct on the
   isolate as `Value::Shared` with one atomic bump (`decode_args_with_shared`,
   `isolate.rs:474`). Each chunk request additionally carries plain `(start, end)` indices
   (§3.3.1). Crossing cost: **O(1) per chunk**, measured flat (~0.15 ms) at any size.
   Elements are read on the isolate zero-copy through the shipped `Shared` readers
   (scalars materialize; containers arrive as `Shared` sub-views — `shared_to_value_shallow`
   and the shared index/len helpers, `src/interp.rs:7298+`).
2. **A plain `Value::Array`**: each chunk's element slice is snapshotted at call time and
   crosses through the airlock as an ordinary structured-clone arg — **byte-for-byte today's
   `worker fn` argument semantics** (cycles handled via `TAG_REF`, `serialize.rs:97`;
   instances reconstructed by class identity with working methods, `serialize.rs` instance
   arm; mutable copies inside the isolate). Total copy cost across all chunks = **one** full
   clone of the input (the chunks partition it), in the same cost class as one freeze walk.
3. **Anything else** — `Value::Shared` of a non-array, a Map/Set/Object/stream, a scalar —
   is a recoverable Tier-2 panic: `task.pmap expects an array or a frozen array (got <kind>)`
   (using the SRV wording convention `frozen <kind>` for a `Shared` non-array). v1 is arrays
   only (§8).

**Decision — unfrozen input is per-chunk copy, NOT auto-freeze.** The drafting brief
recommended auto-freezing internally; verifying the freeze walk against the airlock
(grounding §9) shows auto-freeze is **not behavior-preserving**, and `goal-perf.md`'s PAR
entry sanctions either ("Unfrozen inputs take a freeze-or-copy documented path"). The three
verified semantic deltas that rule auto-freeze out as a silent default:

- **Instances lose their methods.** Freeze converts an instance to a fields-only
  `SharedNode::Instance` (`src/stdlib/shared.rs:228-254`); calling a user method on it inside
  `f` is the SRV §3.8 `method '<name>' is not available on a frozen instance …` panic. The
  airlock instead reconstructs a real instance by class identity (the class ships in the code
  slice) with **working methods**. `pmap(rows, f)` where `f` calls `row.total()` would break
  under silent auto-freeze and works under copy.
- **Element-local mutation inside `f` is legal under copy semantics and a panic under
  freeze.** Under the airlock, `f` owns a private copy — mutating it is sound and isolated
  (exactly what a hand-rolled `worker fn` does today). Auto-freeze would turn that into
  `cannot mutate a frozen object` at runtime, in code that never asked for freezing.
- **Cycles regress.** The airlock supports cyclic inputs (`TAG_REF` visited table,
  Spec A §5 "mandatory, not optional"); `shared.freeze` rejects them by design
  (`shared.rs:279-288`). Auto-freeze would break inputs that work in every other worker call.

So the rule is symmetric and user-visible: **you choose the semantics by choosing the input
form.** A plain array gives today's copy semantics everywhere; `shared.freeze(arr)` — one
explicit line — gives zero-copy crossing **and** the (already-shipped, already-documented)
frozen read-only view semantics inside `f`, identical to how that frozen value behaves
anywhere else in the language. The docs state the perf guidance plainly: *for large
read-only element data, freeze first; for small or mutated-per-element data, pass the plain
array.* Non-sendable contents panic with a field path on **either** path (the airlock's
`value of kind function cannot be sent to a worker at <path>` on the copy path; the same-shaped
freeze message if the user froze) — there is no input form under which a closure inside the
data silently crosses.

### 3.2 Callback shipping: the worker code-shipping closure, verbatim

`pmap` resolves `entry_name = worker_fn_dispatch_name(f)` (panic per §2.2 if `None`), then
builds the code slice **once per call** via `build_code_slice_for_interp(self, &entry_name)`
(`dispatch.rs:919`) — the same `.aso`-aware, engine-shared builder `run_in_worker` uses
(`src/interp.rs:6079`), covering all four modes including `ascript run x.aso`
(`resolve_worker_top_chunk`, `dispatch.rs:874`). The one slice (`entry_aso: Rc<[u8]>`) is
cheaply cloned into each chunk request; the pool-side mirror then ships the bytes **at most
once per isolate** (`Pool::send_to` clears `slice_bytes`/`archive_bytes` for an isolate that
already has them, `pool.rs:132-164`), and the isolate's own `loaded`/`archive_installed`
dedup is the belt-and-braces backstop (`isolate.rs:313-370`). A repeated `pmap` of the same
`f` therefore pays slice-build once per call (a compile of the program source — the
documented existing worker characteristic) and slice-shipping ~never after warmup.

### 3.3 Scheduling: contractual chunk boundaries, native per-chunk driver

#### 3.3.1 The chunk plan (part of the documented contract)

```
cap        = opts.chunks  if given (int ≥ 1)
             else pool cap          // $ASCRIPT_WORKERS if set, else num_cpus (pool.rs:59-64)
chunk_size = max(opts.minChunk ?? 1, ceil(len / cap))
chunks     = ceil(len / chunk_size)
chunk i    = [i * chunk_size, min((i+1) * chunk_size, len))      // i = 0 .. chunks-1
```

- Boundaries are **deterministic given `(len, cap, minChunk)`** and are a **documented part
  of the contract** (the docs publish the formula). This is what makes `preduce` of a
  non-associative `f` *reproducible*: same machine + same env ⇒ same boundaries ⇒ same
  result, byte-identical across runs and across all four engine modes. Cross-**machine**
  reproducibility for a non-associative `f` requires pinning `opts.chunks` explicitly
  (the default `cap` is the machine's pool size) — stated in the docs, demonstrated in tests.
- `chunks ≤ cap ≤ pool size` by default, so per-call dispatch overhead is bounded by the pool
  size (≈ core count), **independent of `len`**.
- **`minChunk` defaults to 1 — deliberately.** A larger default (e.g. 16) was considered and
  rejected: because the chunk count is already capped at pool size, tiny-task overhead is
  bounded at ~`cap × 0.23 ms` regardless of `minChunk`; but a `minChunk > 1` default would
  silently **serialize the small-N / heavy-work fan-out** (8 expensive renders → one chunk of
  8 → zero parallelism), which is precisely `pmap`'s headline use. `minChunk` exists as an
  explicit knob for users whose per-element work is tiny and who want fewer, larger chunks;
  the measured break-even in the bench report (§6) is the guidance for setting it.
- There is **no `opts.workers`** (the brief floated one; rejected §8): the pool is
  process-global and capped by `$ASCRIPT_WORKERS`; `opts.chunks` already bounds this call's
  parallelism, and a per-call pool resize is machinery the pool deliberately does not have.

#### 3.3.2 Chunk execution: a native driver in the isolate loop (no glue bytecode)

Per-element pool dispatch is exactly what PAR exists to avoid, so the per-chunk element loop
runs **natively inside the isolate**, not as synthesized script. `WorkerRequest` gains one
plain-`Send` field:

```rust
/// PAR: when Some, the isolate runs the chunk DRIVER over the decoded data arg
/// instead of a single entry call. Plain Copy/Send scalars — no new wire tag,
/// no serializer change (asserted by tests/par_negative_space.rs).
pub chunk: Option<ChunkJob>,            // src/worker/isolate.rs (WorkerRequest)

pub struct ChunkJob { pub kind: ChunkKind, pub start: u32, pub end: u32 }
pub enum  ChunkKind { Map, Reduce }
```

The request's `args` payload is the encoded one-element array `[data]`, where `data` is the
whole frozen array (frozen path — `start..end` index into it) or the chunk's own element
slice (copy path — `start = 0, end = slice_len`). `isolate_loop` (`isolate.rs:311-418`)
changes minimally: after the existing decode/entry-fetch steps, if `chunk` is `Some` the
select!-raced run future is `run_chunk_job(vm, entry, data, job)` instead of one
`vm.call_value(entry, args)`. The driver:

- **Map:** for `i in start..end`: `elem = element(data, i)`; `r = vm.call_value(entry,
  vec![elem]).await`; collect into a results `Vec` → reply with the encoded results array.
- **Reduce:** `acc = element(data, start)`; for `i in start+1..end`:
  `acc = vm.call_value(entry, vec![acc, elem]).await` → reply with the encoded `acc`.
- `element(data, i)`: a `Shared` array child (materialized scalar or `Shared` sub-view, the
  shipped SRV readers) or a plain array index clone — both engine-shared `Value`-layer reads.
- Per-element control flow mirrors the worker top-level rules **exactly**
  (`isolate.rs:398-414`): `Ok(v)` → the element result (a returned `Value::Future` is driven
  to completion first, the `task.spawn` rule); `Err(Panic)` → the chunk
  replies `WorkerReply::Panic` (first element to panic aborts the rest of the chunk);
  `Err(Exit)` → the existing `exit() is not allowed inside a worker` panic.
  > **CORRECTION (Phase-0 pin, 2026-06-16):** the `Err(Propagate(_)) → nil` arm in the
  > isolate loop is **dead code** — `run_body` (`interp.rs:5452`) converts a body-level
  > `?`-propagation to `Ok([nil, err])` BEFORE `call_value` returns, so a `worker fn`
  > element that ends in `?` yields the `[nil, err]` **pair**, never a raw `Propagate`. The
  > driver keeps the (dead) arm to mirror `isolate_loop` byte-for-byte, but the OBSERVABLE
  > per-element result of a propagating callback is the pair — **identical to a direct
  > worker call / the hand-rolled `gather(map(data, f))`**, which is exactly the
  > venue-invariance (§5.1) the design requires. The §4 table row is corrected to match.
- Inside the isolate, calling the `worker fn` entry runs it **as a plain closure** — the
  `in_isolate()` guard on the VM's higher-order re-dispatch path already guarantees no
  recursive pool dispatch (`src/vm/run.rs:4491-4497`).

Crucially this adds **no new serialization shape**: `ChunkJob` is struct fields on the
already-`Send` `WorkerRequest` (like `caps`, `isolate.rs:118`), never bytes in the
structured-clone stream — the wire tag set is untouched, so the existing
`fuzz/fuzz_targets/worker_serialize.rs` target covers PAR with **zero** changes (asserted,
§5.2). The reply path is also untouched: chunk results return through the normal
`WorkerReply::Ok(bytes, shared)` envelope — a deep copy for plain values, `TAG_SHARED` `Arc`
bumps for any frozen values `f` produced (`isolate.rs:128-133`) — **no new return path**.

#### 3.3.3 `preduce`'s final combine: one more Reduce chunk

After the chunk partials `[p0..pk]` arrive (in chunk order), `preduce` dispatches **one**
final `Reduce` job over the copy-path data `[init, p0, .., pk]` — the driver seeds with the
first element (`init`) and folds `f` across the partials, producing
`f(...f(f(init, p0), p1)...)` in one dispatch (not `k` per-combine pool round-trips of
calling `f` from the caller, and not a caller-side fold — `f` is a `worker fn`; calling it
on the caller would re-dispatch per combine). Total dispatches per `preduce` = `chunks + 1`.
`init` is sendability-checked **up front** (fail fast, before any chunk runs).

### 3.4 The orchestrator: eager future, input-order merge

`task.pmap`/`task.preduce` (in `src/stdlib/task_mod.rs`, routed via the existing `call_task`
dispatcher, `task_mod.rs:55-69`) do, synchronously inside the call: validate args (§2.1/§2.2),
snapshot the input, compute the chunk plan, build the slice once, and dispatch every chunk
(`dispatch_worker` with the `chunk` field — each returns an eagerly-running `Value::Future`,
`src/worker/mod.rs:87-199`). Then they `spawn_local` an orchestrator task (the
`dispatch_worker` bridge pattern: `SharedFuture` + `set_abort`, `mod.rs:164-199`) and return
`Value::Future` immediately. The orchestrator:

1. awaits the chunk futures **in input (chunk) order** — `f0.get().await`, then `f1`, … The
   chunks are already running concurrently (eager dispatch); awaiting in order costs no
   parallelism and is what makes error selection deterministic (§3.5).
2. **Map:** concatenates the decoded chunk result arrays in chunk order — input order by
   construction. **Reduce:** collects partials in chunk order, dispatches the final combine
   (§3.3.3), awaits it.
3. resolves the `SharedFuture` with the merged value (or the first error, §3.5).

### 3.5 Errors, cancellation, panics — deterministic and honest

- **A callback panic** in any element surfaces as that chunk's `WorkerReply::Panic` (message
  preserved verbatim, re-anchored at the `pmap` call span — the existing bridge behavior,
  `mod.rs:180-182`). The orchestrator awaits chunks in input order, so **the reported panic is
  the first failing chunk by INPUT order**, never completion order — if chunk 3 panics first
  in wall-clock but chunk 1 also panics, the program deterministically sees chunk 1's message.
  On the first panic the orchestrator drops all remaining chunk futures.
- **What dropping a chunk future does** (verified, stated honestly): the bridge task aborts →
  the abort `oneshot` sender drops → for a chunk **still queued** on an isolate's FIFO, the
  isolate's `select! { biased; _ = abort => Cancelled, … }` (`isolate.rs:395-397`) observes
  the closed abort **before running it** — the chunk never executes; for a chunk **already
  running**, the abort is observed only when the run future next yields — a CPU-bound chunk
  body that never awaits **runs to completion and its reply is discarded** (the
  `WorkerReply` send fails into a dropped receiver). This is today's pooled-worker
  cancellation semantics, inherited unchanged and documented as such — PAR adds no
  preemption. The `InflightGuard` keeps the pool's load accounting correct on every
  cancellation path (`mod.rs:20-32`).
- **`?`-propagation inside `f`**: per element → the `[nil, err]` **pair** for that element
  (§3.3.2 CORRECTION — `run_body` converts the propagation to `Ok(pair)`; identical to a
  direct worker call), exactly the shipped worker top-level rule. The docs recommend returning `[value, err]`
  pairs as data when per-element fallibility matters (they merge in order like any value).
- **Timeout / race / detach** compose for free because `pmap` returns a normal
  `Value::Future`: `task.timeout(ms, task.pmap(...))` drops the pmap future on timeout →
  cancel-on-drop aborts the orchestrator → chunk futures drop → the cancellation semantics
  above. Dropping an un-awaited `pmap` future cancels the whole operation;
  `task.spawn(task.pmap(...))` detaches it.
- **Worker-isolate hard failures** (isolate terminated, spawn impossible) surface as the
  existing recoverable panics (`worker isolate terminated unexpectedly`); pool-exhaustion
  **graceful degradation** is preserved: a chunk request handed back by the pool
  (`Err(req)`, `pool.rs:87-130`) runs through `run_slice_inline` (`mod.rs:255`), which gains
  the same chunk-driver handling — correct result, just not parallel.

### 3.6 Capabilities: pooled semantics, not a sandbox

Chunks run under the **pooled** `worker fn` capability model unchanged: every chunk request
ships the dispatching isolate's `CapSet` as the read-only floor (`dispatch_worker` fills
`req.caps` from `interp.caps()`, `mod.rs:139`), the pooled isolate installs it fresh per
request and **refuses `caps.drop`** (FFI §4.5a, `isolate.rs:333-342`). `task.pmap` does
**not** create a sandbox and takes no `caps` option — a cap-reduced parallel job is
`run_in_worker(f, input, {caps})` per item/chunk (the dedicated-isolate keystone,
`src/interp.rs:6025-6086`); the docs cross-reference it explicitly.

### 3.7 Results: the normal return airlock, merged locally

Each chunk's results return through the existing reply path (§3.3.2): plain values deep-copy
back; frozen values `f` produced come back as `Arc` bumps via the reply's `TAG_SHARED`
side-vector. The merged `pmap` array is an ordinary **local** (caller-heap) array of those
decoded values; the `preduce` result is an ordinary local value. A non-sendable value
**returned** by `f` (a closure, a generator, a native handle) is the existing encode-time
field-path panic from the isolate — same as any worker return today. No new return transport
exists in v1 (asserted by the negative-space test).

### 3.8 Determinism seams (SP9 / workflow) — inherit the worker posture exactly

Worker dispatch is already **outside** the SP9 record/replay seams: each isolate has its own
per-`Interp` determinism context, and cross-isolate scheduling is the same recorded class of
nondeterminism the async model has (Spec A §9). PAR inherits exactly that posture and adds
nothing: `pmap`/`preduce` results are **order-deterministic data** (input-order merge +
contractual boundaries), so a workflow that calls them replays as faithfully as one that
calls a `worker fn` + `gather` today. The `workflow-determinism` lint's seam tables
(`src/check/rules/workflow_determinism.rs`, `SEAM_CALLS`/`SEAM_MODULES`) do **not** include
worker dispatch and PAR does not add `task.pmap`/`task.preduce` to them — flagging an
order-deterministic data operation would be a false positive by the rule's own zero-FP bar.
The `preduce` contract is stated verbatim in the docs:

> **`preduce` contract.** `f` must be **associative** for `preduce(data, f, init)` to equal
> the sequential `reduce`. Chunk boundaries are deterministic given the input length and the
> chunk count (the published formula, §3.3.1), so even a non-associative `f` is
> **reproducible** — byte-identical across runs and across all engine modes on the same
> machine/configuration — it is just not equal to the sequential fold. The default chunk
> count is the machine's worker-pool size; pass `{chunks: N}` to pin results across machines.

## 4. Failure-mode table (every edge has one verdict)

| Condition | Behavior | Mechanism |
|---|---|---|
| `f` not a named `worker fn` | Tier-2 panic `task.pmap expects a named \`worker fn\` …` | §2.2, `worker_fn_dispatch_name` |
| `f` is a `static worker fn` | same panic (v1 limitation, recorded) | §2.2 |
| `data` not array / frozen array | Tier-2 panic `… expects an array or a frozen array (got <kind>)` | §3.1 |
| empty `data` | `[]` / `init`, pool untouched | §2.1 |
| `opts.chunks`/`minChunk` not a positive int | Tier-2 panic (mirror `task.retry` opts validation) | §3.3.1 |
| non-sendable value inside plain `data` | field-path send panic at chunk encode (fail before dispatch of that chunk; surfaced first-by-input-order like any chunk error) | §3.1 |
| non-sendable `init` | field-path panic up front, before any dispatch | §3.3.3 |
| cyclic plain `data` | works (TAG_REF airlock copy) | §3.1 |
| cyclic data the user tries to freeze | `shared.freeze` cycle panic at the user's freeze call | shipped SRV behavior |
| callback panics on element `j` of chunk `i` | chunk `i` replies Panic; pmap re-raises the first panicking chunk **by input order**; later chunks cancelled | §3.5 |
| `?` propagation inside `f` | that element's result is the `[nil, err]` pair (§3.3.2 correction: `run_body` converts it to `Ok(pair)`) | §3.3.2 |
| `exit()` inside `f` | `exit() is not allowed inside a worker` panic | shipped, `isolate.rs:411` |
| `f` returns a non-sendable | encode-time field-path panic from the chunk | §3.7 |
| caller drops / times out the pmap future | orchestrator aborted; queued chunks never run; in-flight chunks finish-and-discard (documented) | §3.5 |
| pool cannot spawn any isolate | chunk runs inline on the caller (graceful degradation), result identical | §3.5 |
| called from inside an isolate (nested) | same chunk decomposition executed **inline sequentially** (deadlock-free, byte-identical results) | §5.1 |
| `maximum recursion depth` etc. inside `f` | the worker panic surfaces like any chunk panic | shipped |
| mutating a frozen element view inside `f` | `cannot mutate a frozen <kind>` (shipped SRV panic) | §3.1 |
| user-method call on a frozen instance element | SRV's distinct `method '<name>' is not available on a frozen instance …` | §3.1 |

## 5. Correctness — four-mode, fuzz posture, test matrix

### 5.1 Four-mode byte-identity (Gate 1)

`pmap`/`preduce` are `Interp` stdlib methods — engine-shared by construction — and the chunk
bodies execute on isolate **VMs** in all four modes (the tree-walker caller ships the same
recompiled-source slice, `dispatch.rs:815-829`; the `.aso` mode resolves via
`resolve_worker_top_chunk`, `:874`). The shipped workers corpus already proves this pattern
four-mode byte-identical (`examples/workers_parallel_map.as` + the `workers_*.as` set in
`vm_differential.rs`). The determinism levers that make byte-comparison meaningful are
designed in: input-order merge, contractual boundaries, first-by-input-order errors, and the
**nested/degraded paths execute the SAME chunk decomposition** (a nested `preduce` does not
fall back to a plain sequential fold — for a non-associative `f` that would differ from the
chunked result; instead the inline path runs the identical per-chunk + final-combine plan
locally, so venue never changes the value). New corpus examples (§7) join
`tests/vm_differential.rs` in BOTH feature configs; preduce corpus examples use associative
combiners or pin `{chunks}` so CI machines with different core counts agree (§3.8).

### 5.2 Fuzz posture (Gate 15 — asserted, not assumed)

PAR adds **no serialization shape**: no new wire tag, no new `SharedNode` variant, no `.aso`
section — `ChunkJob` rides as plain struct fields beside `caps` on the `Send` request. The
existing `fuzz/fuzz_targets/worker_serialize.rs` therefore already covers every byte PAR
puts on the wire, unchanged. This is pinned by a **negative-space test**
(`tests/par_negative_space.rs`, mirroring `tests/srv_negative_space.rs`): asserts
`ASO_FORMAT_VERSION` is unchanged (read the constant, compare against the pre-PAR value
captured at branch time), asserts the serializer tag count/values are untouched, and greps
that no `Op` variant was added. PAR adds no new engine configuration either — no kill switch
is needed (there is no specialization to kill; the chunk driver is the only path) — but the
differential corpus runs the new examples in all existing modes per Gate 1.

### 5.3 Unit & integration test matrix (Gates 10/14 — happy AND edge)

Rust-level (in `src/stdlib/task_mod.rs` tests via `run_source`, plus `src/worker/` driver
unit tests in the in-process style of `dispatch.rs`'s `run_slice_in_fresh_isolate`):

1. **Order preservation:** `pmap` over 0..N with an `f` whose per-element duration is
   inversely ordered (later elements finish first) still returns input order.
2. **Panic ordering:** chunks engineered so a LATER chunk panics fast and an EARLIER chunk
   panics slow → the surfaced message is the earlier (input-order) chunk's, deterministically;
   also: single panicking element mid-chunk aborts only that chunk's tail.
3. **Empty array / single element:** `pmap([]) == []` with `pool_is_initialized() == false`
   when no prior worker ran; `preduce([], f, init) == init`; `preduce([e], f, init) ==
   f(init, e)`.
4. **Chunk-count edges:** `chunks > len` (clamps to len), `chunks = 1` (sequential-in-one-
   isolate), `minChunk > len` (one chunk), explicit `{chunks}` reproducibility: a
   NON-associative `f` with pinned chunks gives the same (non-sequential) value on every run
   and every mode; the same call with `chunks: 1` equals the sequential fold.
5. **preduce associativity:** associative `f` (`add`) equals sequential `reduce` for several
   `(len, chunks)` combinations including ragged final chunks; `init` non-identity (e.g.
   `init = 100`) still exact (the §3.3.3 once-only `init` property).
6. **Frozen vs unfrozen parity:** same scalar data both ways → identical results; the
   semantic deltas pinned: frozen object element mutation panics / plain copy mutation is
   local and silent; frozen instance method call → the SRV distinct diagnostic.
   > **CORRECTION (Task 2.2, 2026-06-16):** the original "plain instance method works" claim
   > overstated the shipped Spec-A airlock. A class INSTANCE crosses the worker boundary as a
   > **field-only shell** (`resolve_class` ships fields, not methods — a documented Spec A
   > limitation, true of EVERY `worker fn` call, not PAR-specific), so a method call on a
   > plain-instance element raises `value is not callable` while field access works. PAR
   > inherits this unchanged; the battery pins the ACTUAL behavior. Cross-airlock method
   > shipping is a future (Spec B) item, out of PAR scope.
7. **Non-sendable capture/diagnostics:** closure inside plain data → field-path send panic;
   non-`worker fn` callback → the §2.2 panic; `f` returning a closure → encode panic;
   non-sendable `init` → up-front panic.
8. **`?` inside `f`** → the `[nil, err]` pair element (identical to a direct worker call); `[value, err]` pairs merge as data.
9. **Cancellation:** `task.timeout(small, pmap(slow))` returns the timeout pair; queued
   chunks never execute (observable via a per-chunk side-effect file/counter in the isolate —
   assert fewer than `chunks` markers); the pmap future dropped un-awaited cancels.
10. **Nested:** a `worker fn` body calling `task.pmap` runs inline, deadlock-free, identical
    output (including a non-associative pinned-chunks `preduce` — the §5.1 venue-invariance).
11. **Degradation:** the `run_slice_inline` chunk path (forced via the pool's `Err(req)`
    seam in a unit test) produces identical results.
12. **Caps:** a `caps.drop` attempt inside `f` is refused (pooled rule); a denied cap in the
    caller's floor is denied inside chunks.
13. **`--no-default-features`:** `std/task` is core (not feature-gated, `task_mod.rs:1-2`);
    the full battery runs in both configs.

## 6. Performance — measured, same-session, honest

Deliverables (Gates 16/17/18): `bench/data_parallel_bench.as` +
`bench/run_data_parallel_bench.sh` → `bench/DATA_PARALLEL_RESULTS.md`, on the workers-bench
model (`run_workers_bench.sh`):

- **Headline scaling:** `pmap` vs an in-program sequential map over the same CPU-bound `f`
  (LCG-style work, the `WORKERS_RESULTS` workload class), at `ASCRIPT_WORKERS` = 1, 2, 4, 8;
  report wall-clock, speedup, parallel efficiency, and `pmap` vs the hand-rolled
  `gather(map(...))` per-element dispatch (the chunking win itself). Same-session, in-program
  A/B (both arms in one binary, one run) — the SRV MINOR-2 lesson.
- **Break-even sweep (the honest non-goal made a number):** per-element work scaled from
  ~0 (identity) upward; report the per-element duration below which sequential map wins.
  Expectation: with chunk dispatch ~0.23 ms warm and chunks ≈ cores, `pmap` of N elements
  costs ≈ `cores × 0.23 ms` fixed overhead + freeze-or-copy of the input — tiny per-element
  work cannot amortize that; the report publishes the measured threshold and the docs cite it.
- **Frozen vs unfrozen input:** the same `pmap` with `shared.freeze`d vs plain input across
  input sizes (10k → 1M elements): the frozen arm's crossing cost should stay flat per chunk
  (the `SHARED_HEAP_RESULTS` curve) while the copy arm grows linearly; include the one-time
  freeze cost so the report shows the real crossover.
- **`preduce`:** scaling + the `chunks + 1` dispatch overhead visible at small `len`.
- **Gate 12/17:** PAR touches no engine hot path (no dispatch loop, no call path, no value
  representation) — the standard `tests/vm_bench.rs` geomean gate is re-run to prove the
  spec/tw ≥2× floor is untouched, and the negative-space test pins that no engine file
  changed semantics. Peak RSS (Gate 18): `/usr/bin/time -l` rows for the bench workloads,
  watching for the expected per-isolate residency (pool isolates × slice globals) and
  flagging any superlinear growth as a bug.

No speedup is promised in this spec; expectations above are stated and the report decides.

## 7. Implementation surface & cross-cutting checklist

**Runtime/stdlib (the whole change):**
- `src/stdlib/task_mod.rs`: `pmap`/`preduce` exports + `call_task` arms; arg validation; the
  chunk planner; the orchestrator (eager dispatch + `SharedFuture` bridge + input-order
  merge + final-combine stage); the in-isolate inline executor (same decomposition).
- `src/worker/isolate.rs`: `WorkerRequest.chunk: Option<ChunkJob>` (+ the `ChunkJob`/
  `ChunkKind` types); `run_chunk_job` driver; the one-arm change in `isolate_loop`.
- `src/worker/mod.rs`: thread `chunk` through `dispatch_worker` (a private
  `dispatch_worker_job` taking `Option<ChunkJob>`; the public fn passes `None`) and through
  `run_slice_inline`; `dispatch_worker_dedicated` passes `None` (PAR is pooled-only).
- `src/worker/pool.rs`: untouched (the request flows through opaquely; `send_to`'s byte
  suppression is field-agnostic).
- `src/check/std_arity.rs`: register `task.pmap` (min 2) / `task.preduce` (min 3) **iff** the
  curated table covers `std/task` (verify at implementation; it may not — then no entry).
- Checker/inference: no new rule; `task.pmap`/`preduce` synthesize `Unknown` like other
  un-curated stdlib calls — Gate 5 (zero `type-*` on `examples/**`, both configs) re-swept.

**Unchanged (called out so a reviewer confirms the N/A):** grammar (both parsers,
tree-sitter, no regen/pin), formatter, REPL, LSP semantic tokens (no new syntax — LSP
completion of `task` members picks the new exports up from `std_module_exports`
automatically), `.aso` + `verify.rs` (`ASO_FORMAT_VERSION` stays 27), opcodes/disasm/
bcanalysis, the GC (no new `Value` kind), determinism seams (`src/det.rs`), the serializer
tag set, `Vm.instrument`.

**Tests:** the §5.3 matrix; `tests/par_negative_space.rs`; corpus examples below wired into
`tests/vm_differential.rs` (both feature configs) automatically by living in `examples/`.

**Examples (Gate 9 — happy AND edge):**
- `examples/data_parallel.as` — intro: `pmap` over numbers, `preduce` sum, a panicking
  callback caught with `recover`, the empty-array edge, `?`-in-callback → the `[nil, err]` pair.
- `examples/advanced/data_parallel_pipeline.as` — production-shaped: freeze a dataset,
  `pmap` transform with `[value, err]` per-element error handling, `preduce` aggregate,
  `task.timeout` around the whole pipeline, fully error-handled, order-deterministic output.

**Docs (Gate 13):** `std/task` reference — which lives in **`docs/content/stdlib/async.md`**
(there is no `task.md`; `async.md` is the existing home of `task.spawn/gather/race/timeout/
retry` — verified) — gains `pmap`/`preduce` with the chunk-plan formula, the `preduce`
associativity contract (§3.8 wording verbatim), the frozen-vs-plain input guidance, the
break-even citation, and the capability note (§3.6). `docs/content/language/workers.md` gains
a "Data parallelism: `task.pmap`" section cross-linking `shared.md`. **No NAV change** (no
new page; confirm the existing slugs render). `README.md` stdlib table line for `std/task`
mentions pmap/preduce. `CLAUDE.md` (a PAR note under the workers/SRV entries),
`superpowers/roadmap.md`, and `goal-perf.md` (status flip) updated at the end.

## 8. Scope & rejected alternatives

**In scope (v1):** `task.pmap` + `task.preduce` over arrays (plain or frozen); the chunk
plan + `{chunks, minChunk}` opts; the native chunk driver + `WorkerRequest.chunk`; the
input-order merge + first-by-input-order error; the final-combine Reduce stage; inline
nested/degraded execution of the same decomposition; the test matrix, negative-space pin,
examples, docs, bench report.

**Rejected:**
- **`par for` / any syntax.** Stdlib suffices; grammar churn (two parsers + tree-sitter +
  regen + pins + formatter + `.aso`) buys nothing a function doesn't already do. (`goal-perf`
  pillar 3: no surface change.)
- **Auto-freezing unfrozen inputs.** Verified not behavior-preserving (§3.1): frozen
  instance views lose methods, element-local mutation becomes a panic, cycle support
  regresses. The copy path is today's exact worker semantics; freezing is a one-line,
  user-visible opt-in. (This overrides the drafting brief's recommendation, on code
  evidence; `goal-perf`'s "freeze-or-copy documented path" sanctions it.)
- **Rejecting unfrozen input (force-freeze ceremony).** Hostile to the common small-array
  case and inconsistent with every other worker call site, which accepts plain sendable
  values.
- **Work-stealing / dynamic chunk scheduling.** Chunk-static is deterministic (the
  boundaries are the contract that makes `preduce` reproducible) and simple; stealing
  reintroduces completion-order effects PAR's determinism story forbids at the result layer
  and buys nothing until a measured imbalance workload exists. Recorded as future work
  **gated on bench evidence** (a skewed-work benchmark in the report is the trigger to
  revisit).
- **`pfilter` / `peach`** (§2.1). Composable from `pmap` / structurally misleading across
  isolates, respectively. Follow-ups, not v1.
- **`pmap` over Map/Set/streams/generators.** v1 is arrays; others convert explicitly
  (`map.entries()`, `set.values()`, collecting a stream). Recorded follow-up.
- **`opts.workers` per-call pool sizing** (§3.3.1). The pool is global and capped;
  `opts.chunks` is the per-call parallelism lever.
- **Shared MUTABLE state.** Forbidden by the model, forever (SRV §8; Workers Spec A §12).
- **GPU/SIMD.** Out entirely.
- **Per-element pool dispatch** (the status quo pattern as the implementation). ~0.23 ms per
  element warm (`WORKERS_RESULTS.md` §2) — the measured cost PAR exists to amortize.
- **Synthesized glue bytecode for the chunk loop.** A native driver in `isolate_loop` is
  smaller, verifier-untouched, and engine-shared; generating script-level loop fragments
  would re-enter the compiler per call and add a second copy of per-element control-flow
  semantics to keep byte-identical.

## 9. Grounding (verified 2026-06-12; symbols are the anchors, line numbers drift)

- **Pooled dispatch:** `dispatch_worker` `src/worker/mod.rs:87` (sendability gate `:101-110`;
  caps floor shipped `:139`; bridge + cancel-on-drop `:164-199`; `InflightGuard` `:20-32`);
  graceful degradation `run_slice_inline` `:255-324`; dedicated path
  `dispatch_worker_dedicated` `:342` (not used by PAR).
- **Pool:** cap = `$ASCRIPT_WORKERS` else `num_cpus` `src/worker/pool.rs:59-64`; idle → grow
  → least-loaded `:87-130`; the **pool-side slice/archive ship-once mirror** `send_to`
  `:132-164` (the archive-cache commit the brief cites).
- **Isolate:** `WorkerRequest` fields incl. `shared` side-vector and `caps`
  `src/worker/isolate.rs:75-124`; `isolate_loop` `:311-418`; **biased abort select**
  `:393-397` (queued-job cancellation; in-flight runs to next yield); **Propagate→nil /
  Exit-refusal** `:398-414`; per-request caps install + drop refusal `:333-342`;
  `load_slice` `:438`; `decode_args_with_shared` `:474`; `WorkerReply::Ok(bytes, shared)`
  `:128-133`.
- **Code shipping:** closure algorithm module doc `src/worker/dispatch.rs:11-57`;
  `build_code_slice` `:116`; `build_code_slice_for_interp` `:919`;
  `resolve_worker_top_chunk` (`.aso` fallback) `:874`; engine-shared source path doc
  `:815-829`.
- **Engine hooks:** VM `Op::Call` worker arm `src/vm/run.rs:1718-1725`;
  `dispatch_worker_closure` `:549`; the higher-order **in-isolate re-dispatch guard**
  `:4491-4497` (why the chunk driver's `call_value` runs the entry as a plain closure);
  tree-walker worker call `src/interp.rs:5283-5300`; `run_in_worker`
  (`call_run_in_worker`) `:6025-6086`; `worker_fn_dispatch_name` `:7753`.
- **Airlock:** tag set with `TAG_REF = 13` (cycles) and `TAG_SHARED = 15`
  `src/worker/serialize.rs:83-107`; `encode -> (Vec<u8>, Vec<Arc<SharedNode>>)` `:423`;
  `check_sendable` `:116`; `decode_with_shared` `:616`.
- **Frozen heap:** freeze walk + two identity tables `src/stdlib/shared.rs:88-303`;
  **instance freezes to fields-only** `:228-254` (the auto-freeze counter-evidence);
  cycle rejection `:279-288`; non-freezable field-path panic `:256-267`; shared readers
  `src/interp.rs:7298+` (`shared_to_value_shallow`, array len/child helpers).
- **`std/task`:** `call_task` routing `src/stdlib/task_mod.rs:55-69`; `gather` order
  preservation `:114-128`; `SharedFuture`/`spawn_local`/`AbortOnDrop` precedents throughout;
  core (not feature-gated) `:1-2`.
- **Determinism/lints:** `workflow-determinism` seam tables (no worker entries)
  `src/check/rules/workflow_determinism.rs:38-52`; Spec A §9 (cross-isolate timing is the
  documented nondeterminism).
- **Bench evidence:** `bench/WORKERS_RESULTS.md` (scaling 4.98×@8; 0.23 ms warm round-trip;
  1.29 ms/10k-float clone; 83 ms cold first dispatch); `bench/SHARED_HEAP_RESULTS.md`
  (flat 0.15 ms frozen hand-off vs linear clone, 31×@50k; freeze 0.52 ms/10k; the
  same-session-A/B method note).
- **Format/serialization stability:** `ASO_FORMAT_VERSION = 27` `src/vm/aso.rs:167`
  (untouched); `fuzz/fuzz_targets/worker_serialize.rs` (covers the unchanged wire);
  `tests/srv_negative_space.rs` (the negative-space test model).
- **Docs homes:** `docs/content/stdlib/async.md` (the actual `std/task` reference page —
  there is no `task.md`); `docs/content/language/workers.md` + NAV slug
  `'language/workers'` in `docs/assets/app.js:26`.
- **External precedent:** Rayon (`par_iter().map/reduce` — the chunk-then-combine reduce
  shape with a once-only identity/seed); OpenMP static scheduling (deterministic chunk
  assignment as the default; nested-parallel serialization); JDK `Stream.parallel()`'s
  documented associativity requirement on `reduce` combiners — the same contract §3.8
  states.
