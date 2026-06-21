# EXEC — bespoke single-thread executor: A/B results & SHIP verdict

- **Date:** 2026-06-21
- **Machine:** Apple M4 (10 cores), macOS (Darwin 25.5.0)
- **Branch:** `feat/vm-executor` (11 commits off `main` `ff27977c`)
- **Spec:** `superpowers/specs/2026-06-12-vm-executor-design.md` (§7 ship gate)
- **Plan:** `superpowers/plans/2026-06-12-vm-executor.md` (Task 10)

## VERDICT: PARK — ship gate NOT met (async-corpus geomean 0.99x < the ≥1.10 threshold)

The bespoke executor is **fully built, byte-identical, leak-free, and correct** (Tasks 1–9:
vm_differential 448/0 both configs, Miri-clean, no RSS leak, the saboteur-proven scheduling
differential). But the **measured A/B win is ~0%** — far below the spec §7 ship threshold of a
**≥10% async-corpus geomean win**. Per the spec ("no speedup is promised … ship only when the
async-corpus geomean win ≥10% … if it fails, the branch is closed and the numbers recorded —
same as a failed gate"), **EXEC v1 is PARKED with evidence**, not merged. This is an honored,
documented outcome — the JIT/REGION precedent.

## Same-session A/B (`bench/exec_ab.sh`): bespoke (default) vs `ASCRIPT_EXECUTOR=tokio`

Same release binary, executor toggled by env, interleaved per workload so load/thermal drift
cancels. `speedup = tokio_ms / bespoke_ms` (>1 = bespoke faster). 7 runs:

| workload | class | tokio ms | bespoke ms | speedup | tokMB | bespMB |
|---|---|---:|---:|---:|---:|---:|
| `async_inline` | async (target) | 5139 | 5242 | **0.980x** | 13 | 13 |
| `async_concurrent` | async (target) | 3221 | 3194 | **1.008x** | 13 | 14 |
| `spawn_wake` | async (target) | 3432 | 3482 | **0.986x** | 14 | 14 |
| `func_pipeline` | neutral | 1212 | 1203 | 1.007x | 15 | 15 |
| `call_heavy` | neutral | 1212 | 1212 | 0.999x | 13 | 13 |
| `server_request` | neutral | 2232 | 2235 | 0.998x | 14 | 14 |
| `object_churn` | neutral | 7337 | 4927 | 1.489x† | 12 | 13 |
| `json_roundtrip` | neutral | 2955 | 2939 | 1.005x | 13 | 14 |
| `race_compute` | char. | 809 | 807 | 1.003x | 13 | 14 |

> **ASYNC-CORPUS geomean (async_inline · async_concurrent · spawn_wake) = 0.991x.**

† `object_churn` (a NON-async workload, where the executor is never exercised) showing 1.489x is
a single load-spike artifact in the tokio runs, not a real effect — it confirms the harness has
some variance, which is why the async result was re-confirmed below.

### Confirmation re-run (13 interleaved iters, load avg 3.9)

| workload | tokio ms | bespoke ms | speedup |
|---|---:|---:|---:|
| `async_inline` | 5152 | 5133 | 1.004x |
| `async_concurrent` | 3214 | 3186 | 1.009x |
| `spawn_wake` | 3427 | 3499 | 0.980x |

> **ASYNC-CORPUS geomean = 0.998x.** Reproduces the first run: bespoke ≈ tokio, no win.

## Attribution — WHY there is no win (`sample` + `parse_sample.py`, 25s each)

| `async_inline` | async bucket (reactor park / scheduler) | top leaf |
|---|---:|---|
| **bespoke** | **95.9%** | reactor park dominates |
| **tokio** | 94.8% | `kevent` 90.9% |

**Both drivers spend ~95% of self-time in the reactor park.** The bespoke executor did NOT
eliminate the `kevent` park — it is essentially identical to tokio. This is the root cause of
the neutral result, and it is **architectural, not a bug**:

- EXEC chose **Architecture B** (spec §2.3): the bespoke executor runs as ONE ordinary future
  inside tokio's unchanged `LocalSet`. It replaces the task **harness** (a slab insert instead of
  a tokio `RawTask` alloc; a `VecDeque` push instead of a cross-thread wake). That harness cost is
  a *small* slice of `async_inline` (the spawn-alloc + the abort/ref_dec slice).
- But the **dominant** cost — the reactor park between await-resolutions (the `kevent` 90% slice
  the Phase-0/EXEC gate measured) — is **structural to Architecture B**: when the executor's queue
  goes idle (every sequential `await` suspends the root before its spawned body resolves), the
  executor returns `Pending` and **tokio parks on `kevent`**, exactly as before. The executor lives
  *inside* the thing whose park it was meant to avoid.
- The `SharedFuture` await rendezvous also still uses `tokio::sync::Notify` (the "notify" slice),
  which EXEC v1 deliberately did not touch (`SharedFuture` public semantics frozen, spec §8).

## What would be needed for the win (recorded v2-on-evidence — spec §2.2, §8)

The spec already names these as out-of-scope-for-v1 / revisit-on-evidence:
1. **Architecture A** — the bespoke executor OWNS the outer loop and parks on its own rendezvous,
   handing control to tokio's I/O driver ONLY when something can actually arrive from the reactor
   (not on every idle). This is the only way to stop paying the `kevent` park on a pure-compute
   async program that has nothing for the reactor to deliver.
2. **A bespoke same-thread await rendezvous** replacing `SharedFuture`'s `tokio::sync::Notify`, so a
   same-thread resolve→awaiter wake never round-trips through tokio's notify/scheduler.

Both are larger, riskier changes than v1; the spec correctly gated them behind "implement only if B
ships AND a spike proves the delta." B does not clear the ship gate, so neither is pursued here.

## The asset that remains

`feat/vm-executor` is preserved (not deleted), like `spike/wasm-target`. It is a complete,
tested, byte-identical `!Send` executor + the `exec::spawn_local` seam + the `ASCRIPT_EXECUTOR`
kill switch + the §6 differential/Miri/leak batteries — the exact substrate Architecture A (the
v2 win) would build on. The §4 invalidation-style discipline (one spawn chokepoint, one FIFO
queue, the airlock-clean Send waker) is the durable design value, banked for a future evidence-led
v2.

## Gate posture (the non-ship gates that DID hold)

- four-mode + executor-axis byte-identity: `vm_differential` **448/0** BOTH feature configs;
  property **35/0** BOTH; async-weighted fuzz 0 divergences. Bespoke == tree-walker == tokio.
- No RSS leak: un-awaited-async-loop @ 1M iters = 14 MB bespoke ≈ 12.7 MB tokio (bounded, the
  M17 bar). Miri-clean exec core. `dbg_zero_cost_gate` / spec-tw geomean: unaffected (the
  executor is below the VM; the kill switch is the floor).
- The work is correct and mergeable on *quality* — it is parked solely on the **perf ship gate**.
