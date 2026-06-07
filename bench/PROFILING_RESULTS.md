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
