# Task-Scoped Region Allocation — Design (REGION)

- **Status:** **EVIDENCE-REJECTED (NO-GO), 2026-06-16.** The spike was executed (probe → narrow
  prototype → A/B); §5.5 G1 fails decisively — recycled=0 on both gate workloads (json_roundtrip is
  100% native-serde construction; server_request's `resp` is module-scope + a `json.stringify` Call-arg
  sink, statically disqualified). The narrow refcount recycler is sound + byte-invisible (region-on
  `vm_differential` 444/0; `region_escape` recycles 2M) but the gate workloads' allocations are provably
  untouchable by a bytecode-literal recycler. See `bench/REGION_RESULTS.md`; spike frozen on
  `feat/task-regions` (unmerged). _Originally: Draft for review — SPIKE-GATED, not scheduled; this
  document existed so the go/no-go was made against a concrete plan (the JIT-spec §0 posture,
  `superpowers/specs/2026-06-08-baseline-jit-design.md`), not an assumption._
- **Date:** 2026-06-12
- **Code:** REGION (PERF campaign, `goal-perf.md` — the evidence-gated wave)
- **Depends on:** **NANB merged** (the value representation must be FINAL before an allocation
  layer sits under it — `goal-perf.md` REGION row; concretely, the NANB Phase-1 constructor seam
  `Value::object/array/...` is the chokepoint this spec's instrumentation and pool wiring ride),
  **CALL merged** (the call path's ≥3 allocations/call are CALL's target — REGION targets *data*
  allocations and must be measured against the dieted call path, not credited with CALL's win),
  **the LANE Task-0 server workload** (one of the two gate workloads).
- **Depended on by:** nothing.
- **Engines:** the *allocation seam* is engine-shared (`Value` constructors are the one layer
  both engines share, post-NANB), but **region ACTIVATION is VM-only, specialized-mode-only, v1**
  — the tree-walker (and the generic `--no-specialize` VM) stay on the plain allocator forever
  and serve as the always-plain oracle. §6.1 justifies this against Gate 1.
- **Breaking:** **no.** No syntax (region syntax is FORBIDDEN — §8), no semantics, no opcode
  change, no `.aso` change (the analysis side-table is rebuilt from bytecode at load, never
  serialized — the `arith_cache` precedent), no grammar/parser/format/LSP surface.

---

## 0. Read this first — the spike gate IS the design

**`goal-perf.md` states the gate:** *"a spike on `json_roundtrip` + the server workload proving
≥20% allocation-time win without promotion-cost blowback."* Nothing in this document ships until
that spike has run and passed. The precedent is the honored VAL rejection
(`bench/COMPACT_VALUE_RESULTS.md`): a plausible representation idea was built, measured, found
to lose, and **recorded as STOP** — not hidden, not shipped anyway. REGION inherits exactly that
posture, with one addition: this spec's own §2 analysis already **kills the headline design**
(dynamic promote-on-escape) on semantic grounds *before* any benchmark — and records that as a
finding, per the brief's own instruction that "the full design is unsound for AScript's
semantics, the narrow design stands" is a valid and valuable spec outcome.

The decision ladder, stated up front:

1. **§2.1 (identity & aliasing) is the make-or-break analysis.** Verdict, grounded in code:
   object identity and aliasing are first-class observable semantics in AScript — **dynamic
   promote-on-escape by deep copy is UNSOUND** and full reference-forwarding is rejected as a
   moving-collector project. Option (i) is **killed as a finding**, not deferred.
2. **§2.2 (Cc interop)** evaluates the three allocation strategies. (ii) arena-backed `Cc` is
   **verified impossible** with gcmodule 0.3 (code-read of the dependency, §2.2.2). What
   survives is a **narrowed (iii)**: compiler-*chosen*, runtime-*proven* recycling of dead
   container cells (§3) — a degenerate region that needs no value-representation change, no
   promotion, and no new `unsafe`.
3. **§5 is the spike protocol** with exact GO/NO-GO thresholds, including an **early NO-GO
   checkpoint after Phase 0** (the instrumentation may show the gate workloads' allocations are
   structurally out of reach — §5.2 says why that is plausible for `json_roundtrip`).
4. **Either verdict is a first-class outcome.** GO → productionization under the full Gate 1–18
   battery (§6, §7). NO-GO → `bench/REGION_RESULTS.md` + the `goal-perf.md` REGION row flipped
   to evidence-rejected, with numbers (the VAL-rejection precedent, the campaign's "merged OR
   closed with a recorded evidence-based justification" done-clause).

No speedup is promised anywhere in this document. Expectations are stated (§7.2); the number is
whatever `bench/REGION_RESULTS.md` reports.

## 1. Summary & motivation

### 1.1 The structural opportunity

AScript's architecture already draws lifetime boundaries that no shared-heap language has:

- **A task's lifetime is bounded.** A spawned async body's lifetime is bound to its
  `Value::Future` handle — dropping the last handle aborts the task (`src/task.rs:93-101`,
  `HandleInner::Drop` → `AbortHandle::abort`; the spawn sites are `tokio::task::spawn_local` at
  `src/interp.rs:5321/:5908/:5982` and `src/vm/run.rs:1747/:5709/:7762`). A server request
  handler is exactly such a task. The task is a natural region: its garbage is
  overwhelmingly request-shaped — born after task start, dead by task end.
- **Nothing leaves an isolate except copied bytes.** The worker airlock
  (`src/worker/serialize.rs`) is a structured-clone byte serializer — crossing it IS a deep copy
  (promotion-by-construction). `shared.freeze` (`src/stdlib/shared.rs`) is likewise a copy-out
  into a different ownership domain (`Arc<SharedNode>`). Neither boundary can ever observe an
  arena address.
- **The within-isolate escape sinks are enumerable** (§4): function return, upvalue-cell writes,
  `user_globals` writes, stores into longer-lived containers, channel/event sends, the resource
  table, freeze, the airlock. A static analysis has a finite forbidden set to check against.

### 1.2 The measured evidence

`bench/PROFILING_RESULTS.md` (Phase-0 profiling, the campaign's evidence base):

| Workload | Allocation share | Other |
|---|---|---|
| `json_roundtrip` | **allocation 38%** (free/malloc/memmove/bzero) | hashing 11%, gc/refcount 6%, dispatch 12% |
| `object_churn` | **allocation 22%** | dispatch 49%, hashing 13%, gc/refcount 7% |

Allocation is the largest single slice of the JSON glue workload and the second slice of the
object workload. Every short-lived `Cc` container additionally pays the gcmodule tracked-object
tax: a `GcHeader` allocated in front of the box and a thread-object-space linked-list insert on
EVERY tracked `Cc::new` (verified in the dependency source — gcmodule-0.3.3 `src/cc.rs:159-196`:
`RawCcBoxWithGcHeader`, `space.insert(...)`, `Box::leak`), plus cycle-collector scan pressure
proportional to the tracked population (`src/gc.rs:88`, `COLLECT_GROWTH_THRESHOLD = 10_000` —
churn grows the tracked set toward the trigger). Removing malloc/free + header + list-insert +
collector pressure for request-shaped garbage is the prize.

**Honest caveat, stated before the design:** the 38% on `json_roundtrip` is dominated by
allocations made *inside native stdlib code* (`json.parse`/`stringify` building `Value` trees in
Rust), not at bytecode literal sites. A bytecode escape analysis cannot see into `serde`-side
construction. Whether the *eligible* share is large enough is exactly what the Phase-0
instrumentation (§5.2) measures — and an early NO-GO on that number alone is an anticipated,
honored outcome.

## 2. THE HONEST HARD PARTS (read before the design — they shaped it)

This is the riskiest spec in the campaign. The hard parts lead; the surviving design (§3) is
what is left after them.

### 2.1 Identity & aliasing — the make-or-break analysis (and the verdict)

**Question:** is object identity observable in AScript, such that promotion (deep-copying an
arena value into the general heap at an escape sink) would change observable behavior?

**Answer: yes — categorically.** The audit (verified at `src/value.rs`, 2026-06-12):

| # | Operation | Mechanism | Identity-observable? |
|---|---|---|---|
| 1 | `==` / `!=` on `Array`/`Object`/`Map`/`Set`/`Instance`/`Closure` | `gc::cc_ptr_eq` — POINTER identity (`src/value.rs:1414-1419, :1463`; `impl PartialEq for Value` at `:1393`) | **YES** — `a == b` is "same cell", full stop. `[1] == [1]` is `false` by design. |
| 2 | `==` on `Function`/`Bytes`/`Regex`/`Native`/`Enum`/`Class`/`Interface`/`BoundMethod`/`Super`/`Future`/`Generator` | `Rc::ptr_eq` / `SharedFuture::ptr_eq` (`src/value.rs:1414-1469`, `src/task.rs:137`) | **YES** |
| 3 | **Aliasing + mutation** (`let b = a; b.x = 1` observed via `a.x`) | reference semantics on every mutable container — `SetProp`/index-set mutate the shared cell | **YES — this is the language's core data model**, stronger than identity: it is observable even without `==`. |
| 4 | Array search ops built on `==` (`indexOf`/`includes`/…) | inherit row 1 for container elements | **YES** (derived) |
| 5 | `Map` keys / `Set` elements | `MapKey::from_value` returns `None` for every container kind (`src/value.rs:241`) — containers are **not hashable** | **NO** — identity can never be a persistent map key. (One mercy.) |
| 6 | Display/json/msgpack cycle guards, deep-equal `seen` sets | keyed by `cc_addr` (`src/value.rs:1587+`, `src/stdlib/{json,msgpack,object,assert_mod}.rs`) | **NO** — transient within one native call; addresses are never exposed to script. |
| 7 | `shared.freeze` diamond/cycle tables | keyed by `cc_addr` (`src/stdlib/shared.rs`, `in_progress`/`completed`) | Structure-observable (shared subtrees stay shared in the frozen graph), but a sharing-preserving deep copy (cycle-table clone, like the airlock's `TAG_REF`) preserves it. Survivable. |
| 8 | The worker airlock | structured clone with a container-id cycle table (`src/worker/serialize.rs:25-30`) | Already a copy — promotion-by-construction; sharing/cycles preserved on the far side. Survivable. |

**Consequence for promote-on-escape (option (i)).** Promotion deep-copies a subgraph at the
moment one reference escapes. Every *other* live reference to that subgraph (an alias in a
local, a second stack slot, an element of another arena container) still points at the arena
original. After promotion:

- Row 3 breaks outright: `let a = {x:1}; g.cfg = a; a.x = 2` — the global now holds the
  promoted copy, the local mutates the arena original; `g.cfg.x` reads `1` where today it reads
  `2`. **This diverges on the most ordinary code imaginable** — no `==` required.
- Row 1 breaks wherever an escaped value is later compared with a retained alias:
  `g.cfg == a` flips `true` → `false`.

The only sound fix is to **rewrite every live reference at promotion time** — a forwarding
pointer in every arena object header plus a read barrier on every heap access (so stale
references chase the forward), i.e. the moving-collector problem, in a refcounted runtime whose
`Value` words are plain `Cc`/`Rc` pointers spread across fiber stacks, frame slots, upvalue
cells, container interiors, and native-Rust temporaries on the Rust call stack mid-`await`.
There is no safepoint at which the set of references is enumerable (Rust-side temporaries are
invisible to us). **Forwarding is rejected** — it is a different garbage collector, the
explicitly-parked GC-rework campaign, not a spec task.

**Could (i) be saved by narrowing promotion to values "not yet observed for identity"?** No —
row 3 is not an *observation* that can be tracked; aliasing exists the moment a second reference
exists, and a `Value` clone is an untracked refcount bump. The predicate "exactly one reference
exists at the escape point" IS the predicate the narrow design (§3) is built on — at which point
there is nothing left of (i)'s machinery: a uniquely-referenced value needs no copy and no
forwarding; the design collapses into (iii).

> **VERDICT (the recorded finding):** dynamic promote-on-escape via deep copy is **unsound for
> AScript's semantics** (identity via `==` AND aliasing-with-mutation are observable);
> promote-via-forwarding is a moving-collector project, rejected. **Option (i) is dead — not
> spike-gated, dead.** The narrow design stands; it never moves a value, so it has no identity
> hazard by construction (§3.5 hazard 1).

### 2.2 `Cc` interop — the three allocation strategies, evaluated

gcmodule's `Cc<T>` is the allocator for every cycle-capable heap kind today
(`Array(Cc<ArrayCell>)`, `Object(Cc<ObjectCell>)`, `Map`, `Set`, `Instance`, `Closure` —
`src/value.rs:1101+`). A region value cannot *be* a `Cc` without paying `Cc`'s cost — that is
the interop problem.

**(i) A parallel arena-allocated node type behind NANB's value seam.** A tag (NANB's sealed
repr makes room) distinguishes arena pointers from `Cc` pointers; every heap-touching path
branches on it. Cost analysis: the branch lands on the hottest reads in the engine (property
loads, index gets, iteration, `Display`, every stdlib container fn) — the exact paths SHAPE and
the ICs optimize; a mispredictable two-way representation on the receiver defeats the
monomorphic shape guards. And the killer is not the cost: (i) exists to enable *promotion*, and
§2.1 found promotion unsound. With promotion dead, a tagged arena node that *escapes* has no
recourse — so the analysis would have to prove non-escape statically anyway, which is (iii)
with an extra universal tag tax. **Rejected** (unsound parent design + pervasive read-path
cost).

**(ii) Arena-backed `Cc` (custom allocator).** Verified against the dependency source
(gcmodule-0.3.3, `~/.cargo` registry, `src/cc.rs:149-196`): `Cc::new` hard-wires the global
allocator — untracked objects via `Box::into_raw(Box::new(cc_box))`, tracked objects via
`Box::new(RawCcBoxWithGcHeader{...})` + `space.insert(&mut boxed.header, ...)` +
`Box::leak(boxed)`, with the `GcHeader` *linked into the thread-local object space's intrusive
list*. There is no allocator parameter, no `Allocator`-trait plumbing, and the tracked-object
header is owned by the collector's list — an arena could not reclaim it without corrupting the
space. `ref_count()` is `pub(crate)` (`src/cc.rs:379`); `into_raw`/`from_raw` do not exist
(the same absence that gated NANB Candidate A). **Verified NO for gcmodule 0.3 — recorded.**
Upstreaming full allocator support is a gcmodule redesign (the object-space list would need
per-arena segregation), out of scope per the sanctioned "changing Cc/gcmodule itself: rejected
v1".

**(iii) v1-NARROW: static escape analysis + proven-dead recycling (CHOSEN — §3).** Allocate
*nothing* differently; instead, let the compiler prove (or heuristically select, with a runtime
proof — §3.3) container allocations that are dead at a known program point, and **recycle the
`Cc` cell** there instead of letting it round-trip through free/malloc/header/list-insert. The
value stays an ordinary `Value::Object(Cc<ObjectCell>)` its whole life — **no tag branch on any
general path, no mixing problem** (the brief's "proven values never mix… except they DO if
stored into general containers" trap does not arise, because there is no second value class to
mix). The "region" is a per-isolate, task-trimmed pool of dead cells; "bulk-free at task end"
degenerates to "trim the pool at task end" (§3.4). A true bump arena under (iii) was also
considered — a parallel non-`Cc` node type for proven values — and rejected for v1: the moment
a proven value is passed to ANY native fn, stored even transiently, or printed, it must be a
real `Value`, so "never mixes" forces either the (i) tag (rejected above) or an eager copy-in
(promotion again). The recycling shape keeps every win that matters (no malloc/free, no
GcHeader churn, no object-space insert, retained `IndexMap` capacity across reuse) without any
of those traps.

### 2.3 Promotion cost & blowback → recycling's analog: the wrong-proof and the miss

With promotion dead there is no copy-on-escape to blow back. The analogs under (iii):

- **A wrong "dead" verdict** would be catastrophic (two live objects sharing one cell). §3.3
  makes this impossible by construction: the static analysis only *selects sites*; deadness is
  proven at runtime by `ref_count() == 1` immediately before the recycle. A wrong selection
  degrades to a failed check → the normal drop path. Soundness never rests on the analysis.
- **The miss cost** is the real measurable: a per-selected-site branch + refcount load on the
  kill path, and a pool probe on the allocation path. The adversarial high-escape benchmark
  (§5.4 — every constructed object is appended to a retained array, so every check FAILS) is in
  the gate precisely to measure pure miss overhead; the threshold is <5% regression.
- **The Gate-12 default-off cost** is zero by construction: with regions off (env kill switch,
  generic mode, tree-walker) the selected-site bitmap is never built and the handlers take the
  exact pre-REGION path (§6.2 proves it by benchmark).

### 2.4 Async task suspension — memory held across `await`

The pool (and, in any future true-arena variant, the arena) lives across a task's awaits — that
is the point (the task IS the region). A long-lived task must not pin unbounded memory:

- The pool is **capped** (`REGION_POOL_CAP`, default 256 cells per kind class; tunable via
  `ASCRIPT_REGION_POOL_CAP`); a recycle into a full pool falls through to the normal drop.
- **Task-end trim:** the spawn-site guard that already brackets a task's lifetime trims the
  pool at task exit (and the per-request handler path in `http.serve` trims at request end,
  beside the existing `gc::maybe_collect` safe point). A cancelled task (cancel-on-drop) trims
  via the same guard's `Drop` — abort runs destructors, the guard is on the task's own future.
- A recycled cell holds an **emptied** map/vec (capacity retained, contents dropped at recycle
  time) — pooled memory is capacity-only, bounded by cap × max retained capacity; the trim
  releases it. An overflow/fallback event increments a per-Vm stat (`region_pool_overflow`),
  reported by the bench harness (no silent pathology — Gate 18).

### 2.5 The cycle collector — what recycling must not break

- **Arena-internal cycles** never arise: a cell is recycled only at `ref_count() == 1`, and a
  self-referential subgraph (`a.push(a)`, `obj.me = obj`) holds internal strong edges → the
  refcount at the kill site is ≥ 2 → the check fails → the normal drop path → refcount falls →
  the Bacon–Rajan collector reclaims the cycle exactly as today. **No recycled cell can be part
  of a cycle.**
- **Pooled cells remain in gcmodule's object space** (they were never dropped). They hold an
  emptied container → their `Trace` visits nothing → collector scans them in O(pool-cap) — the
  cap bounds it. This is strictly less scan pressure than today's churn (the pool is the steady
  state; today every iteration inserts AND unlinks a tracked object).
- Any future promoted-or-copied value (there are none in v1 — recycling never copies) would be
  built by ordinary constructors and be cycle-traceable as today, trivially.
- **The native-handle no-trace rule is untouched:** recycling applies ONLY to the six
  cycle-capable container kinds; `Native`/FFI/`Shared` handles keep deterministic `Drop` and
  no-op `Trace` (`src/gc.rs:23-47`).

## 3. The surviving design — proven-dead recycling over `Cc` cells (v1-narrow)

### 3.1 Shape

Three pieces, all invisible to the language:

1. **A static site-selection pass** in `src/vm/bcanalysis.rs` (the pure-analysis module —
   reads a `Chunk`, returns facts; the module charter at `bcanalysis.rs:1-18` fits exactly):
   `region_candidates(proto: &FnProto) -> RegionPlan` — for each allocation opcode
   (`Op::NewObject`, `src/vm/opcode.rs:238` / handler `src/vm/run.rs:2296`; `Op::NewArray`;
   v1 starts with `NewObject` only), find kill sites (a `SetLocal` overwrite of the slot the
   allocation was stored to, or a `Pop` consuming it) such that, per the abstract walk, the
   popped/overwritten value is *plausibly* the last reference. The forbidden-op set is the §4
   sink census: any flow into `Return`, `SetUpvalue`/cell slots (`chunk.cell_slots`),
   `DefineGlobal`/`SetGlobal`, `SetProp`/index-set *as the stored value*, spread, any
   `Call*` argument position, `Await`, or `Yield` disqualifies the site. The pass is allowed to
   be heuristic and incomplete (§3.3) — it is a perf chooser, not a soundness proof.
2. **A per-proto side bitmap** `region_kills: Box<[bool]>` (offset-indexed), built lazily at
   first execution (the `arith_cache` side-table precedent — `Chunk.code` stays byte-identical,
   nothing serializes, `.aso` untouched, disassembler/goldens/differential untouched).
3. **A per-Vm `RegionPool`** (`src/vm/region.rs`, new): `Vec<Cc<ObjectCell>>` per kind class +
   stats (`recycled`, `reused`, `overflow`, `miss`). At a flagged kill site, the VM takes the
   dying value; if it is `Value::Object(cc)` and `cc.ref_count() == 1` and the pool has room:
   clear the cell in place (`map.borrow_mut().clear()` — capacity retained; reset
   `shape`/`frozen`), push the `Cc` into the pool. At `Op::NewObject` (region mode only), pop a
   pooled cell if available instead of `Cc::new` — same address-class behavior as a malloc that
   reuses a freed block (§3.5 hazard 2).

### 3.2 The uniqueness probe — the one dependency ask

`Cc::ref_count()` exists but is `pub(crate)` (gcmodule-0.3.3 `src/cc.rs:379`; a fresh `Cc` is
asserted `ref_count() == 1` at birth in `new_in_space`, so the semantics are exactly what the
guard needs). The spike runs against a **3-line read-only getter** exposed via a
`[patch.crates-io]` git fork of gcmodule (a measurement instrument, the same class as a bench
harness). **Production shipping is conditioned on** either (a) the getter upstreamed (a PR in
the same spirit as the NANB Candidate-A `into_raw`/`from_raw` ask — far smaller), or (b) an
owner-noted vendored fork — recorded in the GO criteria (§5.5). This is deliberately NOT
"changing Cc/gcmodule" in the rejected sense: no allocation, ownership, or collection machinery
changes; it is a read-only accessor. If both (a) and (b) are refused, the fallback is a
fully-sound static-only analysis (no runtime guard) — drastically narrower, and it must then
clear a dedicated adversarial battery before any site class is enabled; the spike measures with
the fork either way, so the fallback decision is made with numbers.

### 3.3 The soundness split (the load-bearing idea)

**The static analysis selects; the runtime refcount proves.** A `ref_count() == 1` read on the
dying value at the kill site is a complete deadness proof at that program point — no other
`Value` clone, no container edge, no upvalue, no Rust-side temporary holds it (each would be a
strong count). Therefore:

- The analysis can never cause a wrong answer — its worst failure is a check that always misses
  (pure overhead, caught by the adversarial bench and the `miss` stat).
- Self-edges/cycles fail the check (count ≥ 2) — §2.5.
- The check runs synchronously inside one opcode handler — no `await`, no reentrancy, no
  `RefCell` borrow held across anything.
- Address reuse via the pool is indistinguishable from malloc reusing a freed block — §3.5.

### 3.4 Activation, scope, and the task boundary

- **Modes.** Region mode is active iff: specialized VM (`Vm.specialize == true`,
  `src/vm/run.rs:105`) AND `ASCRIPT_NO_REGIONS` is unset (the permanent kill switch, Gate 15,
  mirroring `--no-specialize`) AND the build has the (post-GO, default-on) region code. The
  tree-walker and the generic VM never activate it — they are the always-plain oracles (§6.1).
- **v1 site classes.** `Op::NewObject` literals only (the spike); productionization (post-GO)
  extends to `NewArray`, then evaluates `Map`/`Set`/`Instance` by the same per-class A/B.
- **The task boundary.** A `RegionScope` guard is created at each `spawn_local` task body
  (`src/interp.rs:5321/:5908/:5982`, `src/vm/run.rs:1747/:5709/:7762` — the same sites the
  Phase-0 probe instruments, §5.2) and at the `http.serve` per-request handler; its `Drop`
  trims the pool toward a floor. Cancel-on-drop tasks trim via the same `Drop` (abort runs
  destructors). The pool itself is per-`Vm` (per-isolate) — recycled cells are proven dead, so
  cross-task reuse within an isolate is harmless; the per-task guard exists for memory bounding
  (§2.4), not correctness.

### 3.5 Why this design has no identity hazard (closing §2.1 for v1)

1. **Nothing ever moves.** A live value keeps one address for its whole life; `==`, aliasing,
   `cc_addr` tables — all untouched. The §2.1 hazards were *promotion* hazards; v1 has no
   promotion.
2. **Address reuse is pre-existing behavior.** A recycled cell's address can be reissued to a
   new object — exactly as the system allocator reissues a freed block to the next `Cc::new`
   today. Every `cc_addr` consumer is transient within one native call (§2.1 rows 6–7), and a
   dead-at-refcount-1 cell cannot coexist with its successor in any single traversal. The
   identity battery (§6.4) pins this with adversarial tests anyway.

### 3.6 The full design (recorded for the record, not for v1)

For completeness, the killed option (i) in one paragraph: per-task bump arenas under NANB's
sealed repr with a tagged arena/`Cc` pointer split; escape sinks (§4) trigger promotion
(sharing-preserving deep copy via a cycle table); arena bulk-freed at task end. It fails on
§2.1 (aliasing/identity — needs forwarding = a moving collector), §2.2 (the universal read-path
tag tax), and §2.3 (copy-on-top-of-allocation blowback on escape-heavy code). It would be
revisitable only after a GC-rework campaign delivers forwarding-capable headers — recorded
here so it is never re-litigated from scratch.

## 4. The escape-sink census (the analysis' forbidden set, verified)

Where a value can leave a task's lifetime — i.e., the ops whose *operand flow* disqualifies a
candidate site (§3.1), and the boundaries the full design would have had to promote at:

| Sink | Code | Notes |
|---|---|---|
| Function return | `Op::Return` / tree-walker `Flow::Return` | the canonical escape |
| Captured upvalue cells | `Vec<Option<Cc<RefCell<Value>>>>` frame cells (`src/vm/fiber.rs:25-31`, `alloc_cells:56`); `SetUpvalue`; cell slots listed in `chunk.cell_slots` | by-reference captures only (capture-by-value slots copy) |
| Module user-globals | `Vm.user_globals` (`src/vm/run.rs:165`), written via `DefineGlobal`/`SetGlobal` (`:5155-:5232`) | the longest-lived in-isolate store |
| Container stores | `SetProp`/index-set/`Append*`/spread — value stored into ANY container | the container may outlive the task; the analysis does not track container lifetimes in v1 → conservative disqualify |
| Call arguments | any `Call*` — the callee may retain | includes every native/stdlib call; conservative disqualify |
| Channel / event sends | `std/events` emitters, `std/sync` channels, actor mailboxes | retain values beyond the sending task |
| Resource table | `Interp.resources` | native state may hold values |
| `shared.freeze` | `src/stdlib/shared.rs` | already a copy-out (Arc domain) — promotion-by-construction |
| The worker airlock | `src/worker/serialize.rs` | already a byte copy — promotion-by-construction |
| `Await`/`Yield` in the live range | suspension publishes the frame to the scheduler arbitrarily long | v1 disqualifies candidates whose live range crosses either |

## 5. The spike protocol (Phase 0–1 of the plan; the gate)

### 5.1 Posture

The cheapest honest probe, two measurements before any engine surgery, with an early-exit
checkpoint between them. Run on the post-NANB/CALL engine (the dependency order exists so these
numbers are not stale).

### 5.2 Phase 0a — allocation-lifetime instrumentation (`region-probe`)

**Goal:** measure the *region-eligible share*: allocations whose lifetime provably ends within
their birth task, split by birth-site class (bytecode literal vs native/stdlib).

**Design (concrete; gcmodule needs NO surgery for this):** the container cells are OUR types
(`ObjectCell` `src/value.rs:24`, `ArrayCell` `:73`, `MapCell`, `SetCell`, `Instance`). Under
`#[cfg(feature = "region-probe")]` (default-off, dev-only):

- each cell gains a `probe_birth: (u64 /*task*/, u8 /*site class*/)` field, set by the NANB
  constructor seam (`Value::object/array/...` — the chokepoint every construction routes
  through post-NANB), with the site class passed by the VM literal handlers
  (`Op::NewObject`/`NewArray` → `Literal`) and defaulted to `Native` elsewhere;
- each cell gains a cfg'd `Drop` impl recording (site class, birth task, died-in-birth-task?)
  into a thread-local `ProbeStats`;
- task identity is a thread-local `CURRENT_TASK: Cell<u64>` + a `TaskGuard` (RAII; restores the
  parent id and marks the task ended) installed at the six spawn sites (§3.4) and the
  `http.serve` request handler — the same seam `RegionScope` will use, so the probe doubles as
  the wiring dry-run;
- "died within its task" ⇔ at drop time the birth task is still live (a thread-local live-set);
  output is a histogram dumped to `$ASCRIPT_REGION_PROBE_OUT` at program end:
  per workload — allocation count and per-kind totals × {literal, native} × {in-task death,
  escaped}.
- **Fallback** (recorded, only if the field+Drop approach measurably distorts what it
  measures): an interposing `#[global_allocator]` bucketing by size class + timestamp — cruder
  (no site class, no task attribution) but zero type surgery.

Run over: the example corpus, `json_roundtrip`, `object_churn`, the LANE Task-0 server
workload.

### 5.3 The Phase-0 checkpoint (early NO-GO)

**Proceed to Phase 1 only if** the region-eligible share — allocations born at a *bytecode
literal site* that die within their birth task — is **≥ 25% of allocation events on at least
one gate workload** (`json_roundtrip` or the server workload). Below that, even a perfect
recycler cannot reach the ≥20% allocation-time gate (allocation is ≤ 38% of total time; the
recycler can only touch the eligible fraction of it). If the share is high only on
`object_churn` (likely — it is literal-shaped by construction), that is recorded but does NOT
satisfy the checkpoint: the campaign gate names `json_roundtrip` + the server workload.
A checkpoint failure is a full NO-GO, recorded with the histogram (§5.6) — the probable cause
(native-side construction dominating `json_roundtrip`, §1.2) is already named here so the
recorded outcome is legible.

### 5.4 Phase 1 — the narrow prototype (one site class)

Implement §3.1–§3.4 for `Op::NewObject` only, behind a `region-spike` cargo feature +
`ASCRIPT_NO_REGIONS`, with the gcmodule `[patch.crates-io]` fork (§3.2). Includes:

- the minimal `bcanalysis` pass (candidate selection; heuristic-grade per §3.3);
- the `RegionPool` + kill-site wiring + stats;
- the **adversarial high-escape benchmark** `bench/profiling/region_escape.as`: a loop
  constructing object literals and appending EVERY one to a retained array (plus a variant
  passing each to a callback) — every kill-site check misses, isolating pure check+probe
  overhead; the friendly benchmarks stay `json_roundtrip`, `object_churn`, the server workload;
- the spike-scope differential: corpus + goldens, tree-walker == specialized(regions) ==
  specialized(`ASCRIPT_NO_REGIONS`) == generic, with a **coverage assertion** (`recycled > 0`
  over the corpus run — the anti-false-green rule, Gate 15).

### 5.5 GO / NO-GO (exact thresholds — all GO criteria required)

Same-session A/B (Gate 16), interleaved, median of ≥5 reps, one machine, shipped profiler as
the attribution instrument:

| # | Criterion | Threshold |
|---|---|---|
| G1 | Allocation-time win, friendly set | allocation-attributed CPU time (profiler) reduced **≥ 20%** on `json_roundtrip` AND the server workload, with end-to-end wall clock improved (not merely re-attributed) |
| G2 | No blowback, adversarial set | `region_escape.as` wall clock regression **< 5%**; `object_churn` not regressed |
| G3 | Zero-cost when off | regions-off (env switch) vs pre-REGION baseline within noise (≈1.00× geomean) on the full bench set; `dbg_zero_cost_gate` re-run green (a dispatch-adjacent change) |
| G4 | Identity analysis sound | the §6.4 identity/aliasing battery green; no differential or fuzz divergence over the spike campaign |
| G5 | Memory | peak RSS not regressed on any gate workload (Gate 18); `region_pool_overflow` bounded on the server soak |
| G6 | Dependency resolved | the `ref_count()` getter upstreamed OR an owner-noted vendored-fork decision recorded (§3.2) |

**GO** → productionization (plan Phase 3+) under the full §6/§7 battery.
**NO-GO** (any criterion missed, or the §5.3 checkpoint) → record and stop (§5.6).

### 5.6 Recording the outcome (both outcomes are first-class)

`bench/REGION_RESULTS.md`: machine/date/commits, the Phase-0 histograms, the full A/B table,
each criterion's measured value, and the verdict in the `COMPACT_VALUE_RESULTS.md` format.
`goal-perf.md`'s REGION row flips to ✅ merged or **evidence-rejected with numbers** (the
honored VAL precedent). On NO-GO the spike branch stays flagged, unmerged (the VAL `c1571ec`
precedent); the Phase-0 probe (a cfg'd-off measurement tool) may merge on its own merits if the
holistic review judges it worth keeping.

## 6. Correctness (if GO — the productionization battery)

### 6.1 Engines & the differential (Gate 1, justified)

Region activation is VM-specialized-mode-only; the tree-walker and generic VM are permanent
plain-allocator configurations. Justification against Gate 1: recycling is a pure
lifetime/address-reuse optimization with zero observable surface (§3.5) — so byte-identity
across all modes remains the assertion, and a region BUG (a wrong reuse) *presents as* a
specialized-vs-oracle divergence, which is precisely the failure class the differential exists
to catch (the same posture as ICs/adaptive arithmetic: fast paths live only in specialized
mode, and "if generic and specialized ever diverge, a specialization GUARD is wrong — fix the
guard"). The residual risk — a bug whose trigger never appears in corpus/fuzz inputs — is
attacked by the coverage assertion + the allocation-heavy fuzz weight (§6.3), not by putting
the oracle on the same allocator (which would blind it).

Differential modes (each in `tests/vm_differential.rs`, BOTH feature configs): tree-walker ==
specialized+regions == specialized+`ASCRIPT_NO_REGIONS` == generic == `.aso`-compiled, over
corpus + goldens, with the **coverage assertion** that the regions-on mode recycled > 0 cells
over the corpus (fail, not warn, post-GO).

### 6.2 Kill switch & zero-cost-off (Gates 12/15/17)

`ASCRIPT_NO_REGIONS` is permanent (not bring-up scaffolding), mirroring `--no-specialize`;
resolved ONCE at `Vm` construction into a plain `bool` so the off-path is a single predictable
branch folded into the existing `specialize` gating. Proven by benchmark (G3) and the standing
`dbg_zero_cost_gate`; the spec/tw ≥2× geomean floor (Gate 17) re-verified at merge.

### 6.3 Fuzzing (Gate 15)

Regions join the differential fuzzer as an axis (regions-on vs oracle) with the generator's
**allocation-heavy grammar weight** raised for this axis (object/array literal density, loops
that churn literals, aliasing patterns: store-then-compare, self-reference, capture). The
fuzz run reports the recycle counter so a silently-cold axis cannot pass green (the JIT spec's
anti-false-green rule). Time-boxed `FUZZ_STRESS_N` campaign on the branch before merge (the
FUZZ ~284k precedent).

### 6.4 The identity/aliasing battery (the §2.1 hard-part-3 tests)

Property + unit tests exercising EVERY identity-observable operation against
recycled-cell-reusing code paths: `==`/`!=` across container kinds before/after a pool reuse at
the same address; alias-mutate-observe through both names with a recycle between iterations;
`indexOf`/`includes` with container elements; Display/json/msgpack cycle guards run over
graphs allocated from reused cells; `shared.freeze` diamond preservation over reused cells;
the worker airlock round-trip; the self-edge case (`obj.me = obj`) asserting the kill-site
check MISSES (stat-asserted) and the cycle collector reclaims (the `gc::tracked_count`
before/drop/after delta). Plus the frozen-flag and shape-reset pins (a recycled cell must never
leak `frozen`/`shape` state into its successor).

### 6.5 Leak & lifecycle tests

Pool cap overflow falls back cleanly (stat-asserted); task cancellation mid-task trims via
guard `Drop` (no growth across a cancel storm); panic unwind frees pools (Drop-based — assert
`tracked_count` returns to baseline); server soak (the V13-T5 pattern) with regions on: live
tracked set stays bounded; REPL session persistence unaffected.

### 6.6 Miri

The v1 recycling core is **safe Rust** (pool of owned `Cc`s + a 3-line upstream getter) — there
is no new `unsafe`, so no new Miri obligation (recorded so the brief's "the unsafe will be
real" expectation is answered: it is real only for a future true-arena variant, which is
exactly why that variant is not v1). If any later phase introduces an arena core, Miri over it
becomes mandatory (the DBG `Code`-newtype precedent, `src/vm/chunk.rs:1020`).

### 6.7 Memory reporting (Gate 18)

Peak RSS (`/usr/bin/time -l`) + allocation counts on the full corpus in `bench/REGION_RESULTS.md`;
pool stats (`recycled`/`reused`/`overflow`/`miss`) reported per workload. A memory regression is
a bug to fix, never a tradeoff to accept.

## 7. Performance (reporting, not promising)

### 7.1 What is measured

Wall clock + profiler attribution (allocation slice before/after) + RSS + allocation counts, on:
`json_roundtrip`, the server workload, `object_churn`, `region_escape.as` (adversarial), the
full bench corpus (no-regression sweep), in specialized/generic/tree-walker modes, same-session
interleaved (Gate 16).

### 7.2 Expectations (stated, not promised)

Plausible win: literal-churn loops and request-handler bodies — each recycled object saves a
free+malloc pair, a `GcHeader` allocation, an object-space link/unlink, and re-derives its
`IndexMap` capacity for free; collector pressure drops with the tracked-set churn.
Plausible miss: `json_roundtrip`, if Phase 0 shows its allocations are native-side (§1.2) —
in which case the recorded NO-GO redirects the allocation problem to native-side construction
(a SHAPE/stdlib concern, e.g. arena-less `Value`-tree building improvements), which would be
the finding, not a failure of measurement.

## 8. Scope & rejected alternatives

**In scope (post-GO):** the §3 recycling design over `NewObject` (+ staged `NewArray`, then
per-class evaluation of `Map`/`Set`/`Instance`); the `bcanalysis` selection pass; the
`RegionPool` + task-trim guards; the kill switch; the §6 battery; the bench report.

**Out of scope / rejected (recorded so they are not re-litigated):**
- **Dynamic promote-on-escape (option (i))** — KILLED by the §2.1 identity/aliasing analysis
  (deep-copy promotion is unsound; forwarding is a moving collector). Not spike-gated — dead.
- **Arena-backed `Cc` (option (ii))** — verified impossible in gcmodule 0.3 (§2.2.2); gcmodule
  allocator surgery is the parked GC-rework campaign's territory.
- **A generational GC** — rejected; the sanctioned GC-rework deferral, a different campaign.
- **Changing Cc/gcmodule machinery** — rejected v1. The ONE sanctioned exception is the
  read-only `ref_count()` getter (§3.2), a dependency ask in the NANB Candidate-A mold, with a
  recorded fallback if refused.
- **Region syntax in the language** — **FORBIDDEN.** Regions are invisible infrastructure;
  performance is never bought with semantics (pillar 3).
- **Cross-isolate regions** — meaningless; the airlock copies by construction (§1.1).
- **A parallel arena value class for "proven" values** — rejected for v1 (§2.2(iii) closing
  paragraph): it reintroduces the (i) tag tax or eager copy-in the moment a proven value meets
  a native call.
- **Per-`Pop` global checks** — the kill-site bitmap confines the check to selected offsets; a
  universal check would be the regression Gate 12 forbids. (A DECODE-side fold of the bitmap
  into the pre-decoded stream is a recorded post-DECODE refinement.)

## 9. Grounding (verified file:line, 2026-06-12)

- `src/value.rs:1101` `pub enum Value` (Cc container variants); `:1393` `impl PartialEq`;
  `:1414-1419/:1463` the `cc_ptr_eq` identity arms (Array/Object/Map/Set/Closure/Instance) —
  the §2.1 verdict's ground; `:189-243` `MapKey` (containers → `None` at `:241`); `:24`
  `ObjectCell` (`map` + `shape: Cell<u32>` + `frozen: Cell<bool>`); `:73` `ArrayCell`.
- `src/gc.rs:152/:159` `cc_addr`/`cc_ptr_eq`; `:88` `COLLECT_GROWTH_THRESHOLD = 10_000`;
  `:113/:137` `collect`/`maybe_collect`; `:126` `tracked_count` (the leak-test seam); `:193`
  `impl Trace for Value`; `:23-47` the traced-vs-deterministic-Drop invariant.
- **gcmodule-0.3.3 source** (cargo registry): `src/cc.rs:149` `Cc::new` → thread-local space;
  `:159-196` `new_in_space` — `Box::new`/`Box::into_raw`/`Box::leak`, `GcHeader` +
  `space.insert` for tracked objects (the option-(ii) NO); `debug_assert_eq!(ref_count(), 1)`
  at birth; `:379` `ref_count()` is `pub(crate)` (the §3.2 ask). `Cargo.toml:46`
  `gcmodule = "0.3"`.
- `src/task.rs:43` `ResultCell`; `:93-101` `HandleInner::Drop` (cancel-on-drop); `:106`
  `SharedFuture`; `:137` `ptr_eq` (Future identity).
- Spawn sites (task-region boundaries): `src/interp.rs:5321/:5908/:5982`,
  `src/vm/run.rs:1747/:5709/:7762` (all `tokio::task::spawn_local`).
- `src/worker/serialize.rs:1-30` the airlock (structured clone, container-id cycle table,
  `TAG_REF`; copies by construction); `TAG_SHARED = 15`.
- `src/stdlib/shared.rs` the freeze copy-out (`in_progress`/`completed` keyed by `cc_addr`).
- `src/vm/bcanalysis.rs:1-18` the pure-analysis charter; `top_level_defs:89`,
  `top_level_statement_starts:149` (the CFG worklist over `verify::op_stack_delta` the escape
  pass extends), `collect_get_global_names:280`.
- `src/vm/fiber.rs:25-31` upvalue cells `Vec<Option<Cc<RefCell<Value>>>>`; `:56` `alloc_cells`.
- `src/vm/run.rs:165` `user_globals`; `:5155-:5232` its write paths; `:2296` the
  `Op::NewObject` handler; `:105` the `specialize` kill-switch doc; `:226` `new_generic`.
- `src/vm/opcode.rs:238` `Op::NewObject`; `:908` `Op::Pop`.
- `bench/PROFILING_RESULTS.md` — allocation 38% (`json_roundtrip`) / 22% (`object_churn`);
  gc/refcount 6–7%; the native-vs-bytecode caveat (§1.2) is this spec's reading of the json
  attribution, adjudicated by the Phase-0 probe.
- `superpowers/specs/2026-06-08-baseline-jit-design.md` §0 — the evidence-gate posture
  mirrored; `superpowers/specs/2026-06-12-nan-boxing-design.md` §4 — the constructor seam this
  spec's instrumentation/wiring rides; `bench/COMPACT_VALUE_RESULTS.md` — the honored-rejection
  precedent; `goal-perf.md` — the REGION gate text, Gates 15–18; `goal.md` — Gates 1–14.
- External precedent: Go escape analysis (allocation-site decisions, never moving — the §3
  shape); Bacon–Rajan (the collector recycling must not fight); the Self/HotSpot forwarding
  literature (why promotion-by-move was rejected, §2.1).
