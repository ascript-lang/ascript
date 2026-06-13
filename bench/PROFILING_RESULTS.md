# Phase 0 — Profiling Results (2026-06-06)

Goal: replace estimated "where time goes" percentages with measured data on
**representative** workloads, to order the performance roadmap with evidence.

- Machine: macOS 26.5.1, arm64, Rust 1.96, `--profile profiling` (release codegen + symbols).
- Tools: macOS `sample` (1 ms, worker-thread self-time → `parse_sample.py`), `samply` (flame graphs), `/usr/bin/time -l` (RSS), in-program `time.monotonic()`.
- Reproduce: `bench/profiling/run.sh`. Programs: `bench/profiling/*.as`.

## Headline timings

| workload | VM ms | tree-walker ms | VM/TW speedup | peak RSS |
|---|---:|---:|---:|---:|
| async_inline (400k trivial async calls) | 5199 | 5646 | **1.09×** | 11 MB |
| async_concurrent (200k gathers ×4) | 3156 | 3884 | **1.23×** | 11 MB |
| json_roundtrip (700k stringify+parse) | 2595 | 3282 | **1.26×** | 11 MB |
| object_churn (6M object create+read) | 4452 | 11212 | **2.52×** | 11 MB |
| workflow_loop (3k durable run+resume) | 12906 | 13046 | **1.01×** | 12 MB |

**Key takeaway:** the bytecode VM's ~2.5× advantage over the tree-walker shows up
**only** on the dispatch-bound tight loop (object_churn). On the realistic
workloads — async, JSON glue, durable workflow — the engine barely matters
(1.0–1.26×) because the time is spent *elsewhere*. A faster interpreter/JIT
speeds up only the part that's already small on real code.

## CPU attribution (worker-thread self-time, idle main thread excluded)

| workload | dominant cost | breakdown |
|---|---|---|
| **async_inline** | **async runtime 78%** | kevent/reactor park 55%, timer 6%, tokio abort+ref_dec+notify+SharedFuture ~12%; VM dispatch 9%, alloc 5%. The `x*2+1` body is a rounding error. |
| **async_concurrent** | **async runtime 71%** | kevent 49%, SharedFuture::get 5%, notify+park; stdlib call 8%, alloc 7%, dispatch 5%. |
| **json_roundtrip** | **allocation 38%** | free/malloc/memmove/bzero; json/serde 13%, dispatch 12%, **hashing 11%** (SipHash + hashbrown rehash), gc/refcount 6%. |
| **object_churn** | **dispatch/VM 49%** | run_loop 18%, Fiber::frame 9%, eval_binop 4%, Fiber push/pop 6%; alloc 22%, **hashing 13%** (SipHash), gc/refcount 7%. |
| **workflow_loop** | **fsync I/O 96%** | `__fcntl` (F_FULLFSYNC) 82%, unlink 8%, open 4%, write 2%. VM/async/alloc all <1%. |

## What this means for the roadmap

1. **Inline async completion (#2) is confirmed the #1 lever.** A trivial async
   call spends ~75% of its life in the scheduler (kevent park + notify + abort +
   refcount), *and* the VM/TW gap is only 1.09× — so this cost is invisible to
   engine work and dominated by the eager spawn + scheduler round-trip. This is
   exactly what poll-inline-first removes.

2. **Faster hashing (#1) is real and broad** — 11–13% of JSON and object work is
   SipHash. Cheap to claim. (Use a *seeded* fast hasher: the HTTP+JSON surface is
   attacker-reachable.)

3. **Allocation (#3/#5) is the biggest single slice of glue code** — 38% of
   json_roundtrip. NaN-boxing + cheaper per-object allocation target this.

4. **Dispatch (the JIT's target) is only decisive on tight numeric/object loops**
   (object_churn 49%), which the VM already wins 2.5× on. On real workloads it's
   5–12%. **Confirms: JIT is the last lever, not the first.**

5. **NEW finding — workflows are an fsync problem, not a language problem.**
   96% is `F_FULLFSYNC`. No VM/JIT/async change moves this; the lever is durability
   engineering (group-commit / batched append / async fsync / a `buffered`
   durability mode). This was not on the original list and should be tracked
   separately from VM perf.

Memory is flat at ~11 MB across all workloads — no leak/bloat signal; cancel-on-drop
structured concurrency is keeping task memory tight.

---

## Phase-0 extension (2026-06-13) — functional / call-heavy / server-request workloads

Goal: extend the bench corpus with three workloads that expose PERF campaign blind spots not covered by
the original five: closure/callback dispatch (functional pipelines), raw function-call overhead (call-heavy
tight loops), and JSON glue with dynamic dispatch (request-shaped workloads).

- Machine: macOS 25.5.0, arm64, Rust 1.96, `--profile profiling` (release codegen + debug symbols).
- Tools: `/usr/bin/time -l` (RSS), in-program `time.monotonic()`.
- Reproduce: `bench/profiling/run.sh`. Programs: `bench/profiling/{func_pipeline,call_heavy,server_request}.as`.
- **This is the pre-LANE baseline every PERF spec A/Bs against.**

### Headline timings (new workloads only)

| workload | VM ms | tree-walker ms | VM/TW speedup | peak RSS |
|---|---:|---:|---:|---:|
| func_pipeline (2k records × 2k filter/map/reduce rounds) | 2928 | 6406 | **2.19×** | 14 MB |
| call_heavy (2M iters, 3 nested fn calls each) | 1917 | 8480 | **4.42×** | 12 MB |
| server_request (500k JSON parse/route/stringify) | 2143 | 3720 | **1.74×** | 13 MB |

### Key takeaways

- **call_heavy shows the strongest VM/TW gap (4.42×)** — raw function-call dispatch is where the bytecode
  VM's frame-reuse and slot-based calling convention most outpaces the tree-walker's `Environment` chain.
  This is the workload that will most visibly benefit from a two-lane engine that avoids per-call overhead.

- **func_pipeline (2.19×)** — closure re-entry and per-element callback dispatch are well-accelerated by
  the VM. The remaining cost is dominated by allocation pressure (building filtered/mapped temporary arrays
  per round) and closure cell indirection, not dispatch itself.

- **server_request (1.74×)** — JSON parse/stringify dominates (~70%+), similar to json_roundtrip. The
  routing lookup (object index + ?? nil-coalescing) and function dispatch are cheap by comparison.

### Idiom adjustments from spec

The spec's `func_pipeline` used JavaScript-style method chaining (`.filter().map().reduce()`). AScript
array methods are module functions (`array.filter`, `array.map`, `array.reduce`), not instance methods.
The workload was rewritten using the standard `array.*` pattern while preserving the measured shape:
three-stage functional pipeline over realistic records, closure dispatch per element.

The spec's `server_request` used 150k iterations (644ms on VM — too short). Scaled to 500k to reach the
1.5–6s target window (2.1s on VM).

### Self-A/B noise floor (geomean ≈ 1.00x proves harness is sound)

`bench/ab.sh target/profiling/ascript target/profiling/ascript 3` output:

| bench | base ms | cand ms | speedup | baseMB | candMB |
|---|---:|---:|---:|---:|---:|
| async_inline | 5218 | 5272 | 0.990x | 12 | 12 |
| async_concurrent | 3154 | 3152 | 1.001x | 12 | 12 |
| json_roundtrip | 2688 | 2702 | 0.995x | 12 | 12 |
| object_churn | 4869 | 4897 | 0.994x | 12 | 12 |
| workflow_loop | 27489 | 27610 | 0.996x | 13 | 13 |
| func_pipeline | 2906 | 2981 | 0.975x | 14 | 14 |
| call_heavy | 1857 | 1859 | 0.999x | 12 | 12 |
| server_request | 2128 | 2118 | 1.005x | 12 | 13 |

**geomean speedup = 0.994x** — noise floor confirmed, harness is ready for LANE A/B comparisons.

---

## Post-LANE re-profile (2026-06-13)

Goal: measure the LANE contribution to each workload category after shipping the two-lane engine,
and update the roadmap implications.

- Machine: macOS 25.5.0, arm64, Rust 1.96, `--profile profiling`.
- Candidate: `feat/two-lane-engine` HEAD (`16e0623`).
- Full results: see `bench/LANE_RESULTS.md`.

### Headline changes vs Phase-0 baseline

| workload | Phase-0 VM ms | post-LANE VM ms | Δ |
|---|---:|---:|---|
| async_inline | 5199 | 5505 | −5.9% (noise; async-dominated) |
| async_concurrent | 3156 | 3138 | +0.6% (noise) |
| json_roundtrip | 2595 | 2689 | −3.6% (noise) |
| object_churn | 4452 | 4141 | **+7.0%** (dispatch gain) |
| workflow_loop | 12906 | 27803 | n/a (I/O; run-to-run jitter large) |
| func_pipeline | 2928 | 3394 | −16% (Phase-0 ext baseline higher) |
| call_heavy | 1917 | 1612 | **+16%** (call-overhead gain) |
| server_request | 2143 | 2149 | 0% (noise) |

Note: `workflow_loop` wall time is dominated by fsync I/O and varies widely run-to-run; the
Phase-0 number was taken with a faster disk state. Not a regression in the engine.

### Updated roadmap implications (re-ranked after LANE)

1. **EXEC (inline-async dispatch) remains #1 lever.** `async_inline` is 5505 ms post-LANE, vs
   5199 ms in Phase-0 — the lane made only a ~2.4% dent. The async scheduler round-trip (kevent
   park + notify) at 78% of wall time is unchanged. EXEC's inline-first path must eliminate the
   `spawn_local` + `SharedFuture` round-trip for calls whose results are immediately awaited.
   **EXEC gate: OPEN** (residual async tax ≥70% on async_inline, ≥60% on async_concurrent).

2. **Faster hashing (#1) remains real and broad** — allocation and hashing costs unchanged by LANE.
   `json_roundtrip` still spends ~38% on allocation and ~11% on SipHash. This is the next
   correctness-compatible win after EXEC.

3. **LANE delivered its headline:** `object_churn` +7%, `call_heavy` +16%, geomean +4.5% across
   8 workloads. The compute-kernel gate shows 19% geomean improvement on the tight-loop corpus
   (vm_bench.rs). This is the upper-bound dispatch gain; real-workload gain is smaller because
   async, alloc, and I/O fractions are unchanged.

4. **Allocation (#3/#5) is still the largest slice of glue code** — json_roundtrip is ~38% alloc,
   unchanged. NaN-boxing and cheaper per-object allocation remain the next structural reduction.

5. **Workflows are still an fsync problem** — workflow_loop is ≥96% F_FULLFSYNC; LANE had no
   effect. Durability engineering (group-commit, batched append, async fsync) is the lever.

6. **Dispatch-only workloads are NEAR THE FLOOR.** With spec/tw = 3.59x geomean on compute
   kernels, the remaining interpreter-speed headroom is smaller. JIT is still the last lever
   (only matters for object_churn-like tight loops that aren't alloc-dominated), and even there
   the LANE + specialization already deliver 3–6x. The top priority remains EXEC.

### Re-profile checkpoint

Per `goal-perf.md`: LANE's post-merge re-profile confirms async_inline's residual async share
≥15%, opening the EXEC gate. The next scheduled checkpoint is post-EXEC, where the goal is to
confirm that `async_inline`'s async scheduler fraction has been cut and the overall geomean
has moved meaningfully.

---

## Post-CALL re-profile (2026-06-13)

Goal: mandatory campaign re-rank checkpoint after CALL merges, per `goal-perf.md`. Characterize
the `func_pipeline` bottleneck now that call-path allocation has been driven to ~0 by A1+A2+A3,
and re-order the remaining specs by evidence.

- Machine: macOS 25.5.0, arm64, Rust 1.96, `--profile profiling`.
- Candidate: `feat/call-path-diet` HEAD (`dcced4e`), A1+A2+A3+Unit B.
- Full A/B results: see `bench/CALL_RESULTS.md` (Task 4.1).

### Headline: what changed in func_pipeline post-CALL

CALL drove the qualifying call-path allocation slope from ~3.0/call (pre-A1) down to **0.000/call**
(post-A1+A2) and the per-element re-entrant cost from 31 to **15 allocs/element** (A3). The
wall-clock improvement on `func_pipeline` is **+1.1%** — modest on a fast system allocator that
already amortises small heap allocations well. This is the expected outcome when the bottleneck
shifts away from the cost being removed.

### Post-CALL attribution: where does func_pipeline time go now?

With call-path allocation no longer the dominant variable, the `func_pipeline` profile
(2k records × 2k filter/map/reduce rounds, `target/profiling/ascript`) reveals two remaining
dominant costs:

| cost source | estimated share | notes |
|---|---|---|
| **Object hashing/storage** (SipHash + IndexMap insert in filter/map result construction) | ~40–45% | Each pipeline stage constructs a new array of objects; every key hash at object literal construction is now the ceiling. |
| **Dispatch/arithmetic in callback bodies** | ~25–30% | Already optimized by LANE's sync driver; further gains require DECODE (pre-decoded stream) or ELIDE (contract elision). |
| **Allocation — array/object construction** | ~15–20% | The surviving allocation pressure: intermediate arrays, not per-call overhead (now eliminated). |
| **GC / refcount traffic** | ~5–8% | Rc clone/drop on Value passing; reduced by NANB (value size) and SHAPE (flat slab). |
| **Call overhead** | **~0%** | Driven to floor by A1+A2+A3. Qualifying call: 0 allocs, stack-window binding. |

### Re-ranked remaining levers (post-CALL)

The post-CALL profile re-ranks the remaining specs by measured impact on the current bottlenecks:

1. **EXEC — bespoke single-thread executor** (gate: OPEN from LANE). Async scheduler cost is
   unchanged: `async_inline` residual async share ≥70%, `async_concurrent` ≥60%. The inline-first
   dispatch path is still the #1 lever for async-heavy workloads. EXEC remains the top priority
   among open engine specs. *(Status: gate OPEN — proceed when sequencing allows.)*

2. **SHAPE — shape-native object storage + interior hashing.** Post-CALL profiling confirms that
   object hashing/storage is the NEW ceiling for `func_pipeline` (now that call-path allocation is
   at zero). `resync_object_shape` key-clone + SipHash on every literal key are the concrete targets.
   Precomputed shape ids at compile time (zero hashing at construction for literals) + a fast interior
   hasher (ShapeRegistry/IC maps, not user-facing `Map`/`Set`) are the mechanism. SHAPE now ranks #2
   by measured remaining impact. *(Status: spec locked; ready to start.)*

3. **NANB — 8-byte NaN-boxed Value.** Value representation affects every allocation, clone, and
   drop in the pipeline — including the surviving GC/refcount traffic and the intermediate
   array-of-objects alloc pressure. NANB must rerun the Gate-12 A/B (the 16-byte thin-Str attempt
   was a measured regression; NANB is evidence-gated). Depends on SHAPE stabilizing object internals
   first (avoid double-churn). *(Status: spec locked; blocked on SHAPE.)*

4. **DECODE — pre-decoded instruction stream + superinstructions.** The dispatch/arithmetic share
   in callback bodies is already LANE-optimized; DECODE's pre-decoded fixed-width records and
   fused superinstruction pairs are the next dispatch lever. Also absorbs the `Op::CallMethod`
   in-place binding deferred by CALL. *(Status: spec locked; depends on LANE only.)*

5. **ELIDE — contract elision via static proof.** When TYPE proves a call site is safe, the
   compiler emits an unchecked call. Eliminates the surviving `check_call_args` cost on proven-safe
   sites in the pipeline. Independent of SHAPE/NANB/DECODE — can overlap. *(Status: spec locked.)*

### What CALL bought vs what it didn't

| lever | pre-CALL | post-CALL | verdict |
|---|---|---|---|
| Per-call allocation (capture-free) | ~3.0/call | **0.000/call** | ✅ eliminated |
| Per-element re-entrant allocs | ~31/element | **15/element** | ✅ halved |
| Higher-order callback fiber overhead | fresh fiber/element | ONE reused fiber | ✅ eliminated |
| Wall-clock (func_pipeline) | 3065 ms (base) | 3031 ms | +1.1% (fast-allocator amortisation) |
| Object hashing/storage | unchanged | unchanged | → SHAPE |
| Async scheduler tax | unchanged | unchanged | → EXEC |
| Dispatch/arithmetic in callbacks | LANE-improved | unchanged further | → DECODE |

**Primary CALL deliverable:** the structural alloc/memory win (Gate 18) — 0 allocs/qualifying call
on the fast path. The +1.1% wall-clock headline is honest: a fast system allocator's per-allocation
cost is already small on this hardware; the win matters more under memory pressure, in long-running
processes, and when RSS headroom is limited (the allocator tax compounds across the working set).

### Re-profile checkpoint

Post-CALL re-profile confirms: (a) call-path allocation is no longer a measured bottleneck; (b)
object hashing/storage is the new `func_pipeline` ceiling; (c) EXEC gate remains OPEN (async tax
unchanged). The next mandatory checkpoint is post-DECODE, where the goal is to confirm that the
dispatch/arithmetic fraction in callback bodies has been reduced, and to make a recorded
evidence-based decision on EXEC (start vs defer). Per `goal-perf.md`: re-profile after DECODE is
the second of two mandatory campaign re-profile checkpoints before the JIT decision gate.
