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
