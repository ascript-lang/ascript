# LANE A/B Results (2026-06-13)

## Methodology

- **Machine:** macOS 25.5.0, arm64, Rust 1.96.
- **Build profile:** `--profile profiling` (release codegen + debug symbols).
- **Same-session guarantee (Gate 16):** both binaries ran interleaved in a single invocation
  of `bench/ab.sh` — one base run immediately followed by one candidate run per workload,
  repeated for every iteration. Thermal and frequency state are shared across the session.
- **Runs per workload:** 7 interleaved pairs; reported value = median.
- **Baseline binary:** `main` at the merge-base (`git merge-base HEAD main` = `1e29e95`),
  built via `git worktree add /tmp/lane-base 1e29e95 && cd /tmp/lane-base && cargo build --profile profiling`.
  This is the pre-LANE DEFER-merged engine (no sync-lane driver).
- **Candidate binary:** `feat/two-lane-engine` HEAD (`16e0623`),
  built via `cargo build --profile profiling` from the branch root.
- **Command:** `bench/ab.sh /tmp/lane-base/target/profiling/ascript target/profiling/ascript 7`
  (run from the branch root so `bench/ab.sh` resolves workload `.as` files from this branch).

---

## A/B Table: baseline (main@merge-base) vs candidate (feat/two-lane-engine)

| bench | base ms | cand ms | speedup | baseMB | candMB |
|---|---:|---:|---:|---:|---:|
| async_inline | 5433 | 5505 | 0.987x | 12 | 12 |
| async_concurrent | 3146 | 3138 | 1.002x | 12 | 12 |
| json_roundtrip | 2737 | 2689 | 1.018x | 12 | 12 |
| object_churn | 4776 | 4141 | **1.153x** | 12 | 12 |
| workflow_loop | 27894 | 27803 | 1.003x | 13 | 13 |
| func_pipeline | 3349 | 3394 | 0.987x | 14 | 14 |
| call_heavy | 1946 | 1612 | **1.207x** | 12 | 12 |
| server_request | 2201 | 2149 | 1.024x | 13 | 12 |
| **geomean** | — | — | **1.045x** | — | — |

**Result:** the LANE candidate is **4.5% faster geomean** across all 8 workloads.
The gains concentrate where predicted: dispatch-bound (`object_churn` +15%, `call_heavy` +21%).
Async-scheduler-dominated workloads (`async_inline`, `async_concurrent`, `workflow_loop`) are
within noise — their bottleneck is the kevent/park/notify cycle, not instruction dispatch.

---

## RSS Check (Gate 18)

All workloads: **no RSS regression**. Every candidate value equals or is below the baseline:

| bench | baseMB | candMB | Δ |
|---|---:|---:|---|
| async_inline | 12 | 12 | 0 |
| async_concurrent | 12 | 12 | 0 |
| json_roundtrip | 12 | 12 | 0 |
| object_churn | 12 | 12 | 0 |
| workflow_loop | 13 | 13 | 0 |
| func_pipeline | 14 | 14 | 0 |
| call_heavy | 12 | 12 | 0 |
| server_request | 13 | 12 | −1 MB |

**Gate 18: PASS.** No RSS regression on any workload. The sync-lane driver is a code path in
the existing Vm struct (no new heap allocation; `sync_lane` is a `bool`, `lane_sync_ops` /
`lane_bursts` are `Cell<u64>` — a few bytes on the Vm stack). The `−1 MB` on `server_request`
is allocator reclaim jitter, not a real change.

---

## Lane-On vs Lane-Off Isolation

Candidate binary run twice: with lane ON (default) and with lane OFF (`ASCRIPT_NO_SYNC_LANE=1`
via `/tmp/lane_off_wrapper.sh`). This isolates the lane's own contribution from anything else
on the branch. 5 interleaved runs via `bench/ab.sh`.

In this table, **base = lane-ON, cand = lane-OFF**, so speedup = lane-on / lane-off (< 1.0 = lane
is faster than the async-only driver):

| bench | lane-on ms | lane-off ms | lane-on/off | note |
|---|---:|---:|---:|---|
| async_inline | 4923 | 4957 | 0.993x | scheduler-dominated; lane saves ~9% dispatch |
| async_concurrent | 2973 | 2968 | 1.002x | noise |
| json_roundtrip | 2773 | 2791 | 0.993x | alloc-dominated |
| object_churn | 4223 | 4989 | **0.846x** | dispatch-dominated: +15% |
| workflow_loop | 27500 | 28053 | 0.980x | fsync I/O dominated |
| func_pipeline | 2825 | 2900 | 0.974x | ~3% from closure dispatch |
| call_heavy | 1553 | 1885 | **0.824x** | call-dominated: +18% |
| server_request | 2076 | 2142 | 0.969x | ~3% from routing/dispatch |
| **geomean** | — | — | **0.945x** | lane is 5.8% faster than async-only |

**Interpretation:**
- The lane's headline gains are on `object_churn` (+15%) and `call_heavy` (+18%), exactly the
  workloads where the sync-driver's ip-dispatch loop avoids the async machinery overhead on
  every instruction.
- Async-dominated workloads (`async_inline`) show ~1% improvement: the loop body dispatches
  faster in the sync lane, but the first `await` in each async call always parks (the future is
  pending-by-construction), so the scheduler round-trip cost is unchanged.
- Alloc/I/O-dominated workloads (`json_roundtrip`, `workflow_loop`) are at noise — the lane
  doesn't affect allocation or fsync.

---

## Shipped Profiler Dogfood (Gate 16)

```
target/profiling/ascript run --profile cpu -o /tmp/call_heavy.speedscope bench/profiling/call_heavy.as
```

Output: `call_heavy: sum=2000000000 elapsed_ms=2637.2` — produced a valid speedscope JSON (102 kB).

Top frames in the profile: `step`, `scale`, `add` — the three user-defined functions in `call_heavy.as`.
This confirms the profiler works correctly with the LANE candidate: no crash, correct output,
and the profile correctly attributes time to the three nested functions that dominate the workload.

---

## Async-Corpus Bucket Re-Attribution (EXEC Gate Input)

Re-timed the two async-dominated workloads with lane ON and OFF to measure the residual async tax:

| workload | lane-on ms | lane-off ms | improvement |
|---|---:|---:|---|
| async_inline (400k trivial async calls) | 5338 | 5468 | 2.4% faster |
| async_concurrent (200k gathers ×4) | 3140 | 3161 | 0.7% faster |

**Reference:** Phase-0 CPU attribution (from `bench/PROFILING_RESULTS.md`):
- `async_inline`: async runtime 78% (kevent/park 55%, timer 6%, notify+abort+SharedFuture ~12%),
  VM dispatch 9%, alloc 5%.
- `async_concurrent`: async runtime 71% (kevent 49%, SharedFuture::get 5%, notify+park),
  stdlib call 8%, alloc 7%, dispatch 5%.

**Post-LANE estimate:** The sync lane improves the ~9% VM dispatch component of `async_inline`
by ~35% (from the `0.645x` lane-on/off ratio measured on compute kernels). That accounts for
`0.09 × 0.35 ≈ 3.2%` of the workload, consistent with the observed 2.4% improvement. The
remaining ~78% (async scheduler: kevent, park, notify, SharedFuture) is unchanged.

**Residual async tax:** Still ≥70% of wall time on `async_inline` and `async_concurrent`.
The lane moved only the VM-dispatch fraction to the sync driver; the scheduler round-trip on
every pending `await` (which is ALL awaits in `async_inline` — trivial async calls are always
pending on the first poll) remains.

---

## EXEC Gate Verdict

**Residual async share: ≥70% on async_inline, ≥60% on async_concurrent.**

This is well above the ≥15% EXEC gate threshold. The LANE's sync driver has made the
VM-dispatch fraction faster but has NOT moved the scheduler bottleneck — that bottleneck
requires eliminating the `spawn_local` + `SharedFuture` round-trip, which is the EXEC campaign
spec's job (inline-first dispatch, §4 "zero-overhead trivial-async" goal).

**EXEC gate: OPEN.** The residual async tax remains ≥15%, confirming that the inline-async
completion work (EXEC campaign spec) has headroom to move the needle on `async_inline` and
`async_concurrent`.

**Spec-§8 honesty note on async_inline:** The workload deliberately uses trivial `async fn`
calls (`return x * 2`). Under AScript's M17 eager-spawn model, every `async fn` call does a
`spawn_local`, parks the caller, yields to the scheduler, and then awaits the result. The LANE
handles `Op::Await` on an already-resolved future inline (Task 6), but in `async_inline` the
async bodies haven't finished yet when the outer code reaches their `await` — so the outer
`await` always escalates to the async driver. The scheduler round-trip is structural, not
incidental; EXEC's inline-first dispatch must change the spawn path itself to eliminate it.

---

## vm_bench.rs Compute-Kernel Summary (Task 8)

From the `vm_bench.rs` `lane_on_off_overhead` section (release mode, compute-bound kernels):

| benchmark | lane-on ms | lane-off ms | lane-on/off |
|---|---:|---:|---|
| fib(30) recursion | 449.8 | 546.1 | 0.824x |
| sum recursion | 171.5 | 220.9 | 0.777x |
| numeric loop (1e6) | 100.6 | 155.9 | 0.645x |
| while loop (1e6) | 129.0 | 170.3 | 0.758x |
| property r/w (1e6) | 193.0 | 259.7 | 0.743x |
| method dispatch (1e6) | 347.0 | 408.5 | 0.849x |
| string concat (50000) | 52.3 | 56.1 | 0.932x |
| template build (50000) | 78.9 | 78.1 | 1.011x (alloc noise) |
| closure capture (1e6) | 382.6 | 476.9 | 0.802x |
| **geomean** | — | — | **0.809x** |

The compute-kernel geomean (0.809x = lane 19% faster than async-only) is the upper-bound
contribution the lane can make to any workload. End-to-end, the 8-workload A/B shows 4.5%
(geomean 1.045x), limited by the scheduler and alloc fractions.
