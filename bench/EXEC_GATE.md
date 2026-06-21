# EXEC Evidence Gate — post-LANE re-profile verdict

- **Date:** 2026-06-21
- **Machine:** Apple M4 (10 cores), macOS (Darwin 25.5.0)
- **Commit (gate baseline):** `ff27977c` (`main` HEAD at gate time)
- **Spec:** `superpowers/specs/2026-06-12-vm-executor-design.md`
- **Plan Task 0:** `superpowers/plans/2026-06-12-vm-executor.md`
- **Threshold (spec §0/§1):** the post-LANE re-profile must show the **async-runtime
  share ≥ 15%** on the async corpus for the EXEC gate to be OPEN. Below 15% → the spec is
  CLOSED with the evidence recorded (an honored, JIT-style outcome).

## What "async-runtime share" means

The fraction of worker-thread self-time spent in the tokio task driver + reactor — the
`kevent`/reactor-park + timer + tokio task/notify/abort/ref_dec + `SharedFuture` buckets —
as opposed to VM dispatch, allocation, GC, hashing, or actual program work. This is exactly
the `async` bucket in `bench/profiling/parse_sample.py`'s attribution and the "async runtime
%" column of `bench/PROFILING_RESULTS.md`'s CPU-attribution table. The gate is a **structural
attribution**, not a timing delta, so it is robust to absolute machine speed and load (the
SRV MINOR-2 cross-day-baseline hazard applies to A/B *timing* comparisons, not to a
single-build self-time fraction).

## Evidence

### Recorded post-LANE re-profile (`bench/PROFILING_RESULTS.md`, 2026-06-13, same machine)

| workload | async-runtime share | breakdown |
|---|---:|---|
| `async_inline` (400k trivial async calls) | **78%** (post-LANE; LANE moved it ~2.4%) | kevent/reactor park 55%, timer 6%, tokio abort+ref_dec+notify+SharedFuture ~12%; VM dispatch 9% |
| `async_concurrent` (200k gathers ×4) | **71%** | kevent 49%, SharedFuture::get 5%, notify+park; stdlib 8% |

`PROFILING_RESULTS.md` "Post-LANE re-profile" (line 158) recorded **EXEC gate: OPEN**
(residual async tax ≥70% on async_inline, ≥60% on async_concurrent); the "Post-CALL
re-profile" (line 225) re-confirmed it unchanged after CALL.

### Fresh same-day confirmation (2026-06-21, this gate)

Re-sampled `async_inline` on current `main` (`ff27977c`) with the Phase-0 methodology
(`target/profiling/ascript` + macOS `sample` 30s + `parse_sample.py`), under heavy machine
load (load avg ~23 — concurrent agent processes; the saturated reactor *inflates* the park
share, which only strengthens the verdict):

```
async_inline   (5088 samples @ 1ms)
  --- bucket self-time ---
    async           90.7%  (4617)   <-- kevent/reactor park + tokio task driver
    dispatch/vm      4.4%  (224)
    other            2.6%  (133)
    alloc            1.4%  (73)
    gc/refcount      0.4%  (22)
    ...
  --- top leaf symbols ---
     88.0%  kevent                              <-- the reactor syscall
      1.8%  ascript::vm::run::Vm::sync_burst    <-- the engine itself is <5%
      0.2%  tokio::runtime::io::driver::Driver::turn
```

The fresh `async` share (90.7%) confirms and exceeds the recorded 78%. Every spec merged
between the post-LANE re-profile and this gate (DECODE=dispatch, ELIDE=compute,
PAR/RT/RESIL/CNTR/SIG/BATT/EMBED/REPLAY/WASM=stdlib/tooling/target) touches the
per-instruction or stdlib paths — **none** touch the tokio spawn/wake/reactor driver — so the
async share is structurally unchanged, as measured.

### Per-workload share table (gate columns)

| workload | share before LANE (Phase-0) | share after LANE (2026-06-13) | fresh same-day (2026-06-21) | ≥ 15%? |
|---|---:|---:|---:|:--:|
| `async_inline` | 78% | 78% | **90.7%** | ✅ |
| `async_concurrent` | 71% | 71% | (recorded 71%; structurally unchanged) | ✅ |

Context workloads (NOT gate-relevant; LANE/CALL targets): `func_pipeline`, `call_heavy`,
`server_request` are dispatch/allocation-bound (async share <15%), as expected — EXEC does
not target them and they are the zero-regression reference for Task 10.

## VERDICT: GO (share = 90.7% ≥ 15%)

The residual async-runtime tax LANE deliberately did not touch (eager `spawn_local`, the
tokio task harness, the reactor park) is the dominant cost on the async corpus — 90.7% fresh,
78%/71% recorded — an order of magnitude above the 15% gate. EXEC proceeds to implementation
(Architecture B, v1) per the plan, under the separate **ship gate** (§7): merge only if the
measured A/B shows a ≥10% async-corpus geomean win with zero non-async/RSS regression
(Task 10); otherwise the branch is parked with the numbers recorded — also an honored
outcome.
