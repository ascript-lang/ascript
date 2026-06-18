# RESIL — `std/resilience` performance results

Gates 12 (zero-cost when unused), 16 (same-session A/B), 17 (≥2× spec/tw floor), 18 (RSS).
All numbers same-machine, same-session, **release** builds. RESIL is pure stdlib + a CORE
task-local seam (`TASK_LOCALS`); it bumps no `.aso` version and adds no opcode.

## The zero-cost question

RESIL's `TASK_LOCALS` seam (deadline + trace id) has **no in-binary off-state** — unlike DBG's
`instrument == None` toggle, the task-local cell is always compiled in and the "off" state is
simply *no deadline/trace set*, which routes every consult through the `None` branch
(`task_locals_capture()` / `deadline_remaining_ms()` → one TLS `try_with`, no clone). So the
genuine zero-cost proof is **cross-binary** (pre-RESIL `main` @ `11a5d7d` vs this branch), which
is what Gate 16 below measures. The in-process `resil_zero_cost_gate` section
(`tests/vm_bench.rs`) re-asserts the Gate-17 floor in one binary.

## Gate 16 — cross-binary A/B (pre-RESIL `main` vs branch), async-spawn-heavy

Worst-case workload: **1,000,000** eager `spawn_local`s of a no-op `async fn` with NO deadline/
trace set (`/tmp/resil_spawn_bench.as`), so every spawn pays the spawn-site
`task_locals_capture()` (`try_with` + `None`-clone) + the `task_locals_scope` wrap. This is the
pathological case — a real workload's spawned tasks do actual work that dwarfs the capture.

| metric | pre-RESIL `main` | branch | ratio |
|---|---|---|---|
| wall-clock (median of 5) | 12.96 s | 13.27 s | **1.024×** |
| user CPU | 0.93 s | 1.01 s | 1.086× |
| sys CPU | 1.78 s | 1.78 s | 1.00× |

**Verdict: PASS** against the established DBG zero-cost bound (1.05× wall-clock). The wall-clock
delta is 2.4% on a pure-spawn-spam microbench (the workload is idle-dominated: ~13 s real vs
~2.8 s CPU, so wall-clock is the runtime's reactor/scheduling, not RESIL). The cleaner CPU signal
is **user CPU +8.6%** — i.e. the per-spawn task-local capture costs ~80 ns of user CPU over a
million bare spawns. On any workload where a spawned task does real work, this is unmeasurable.
Honest framing (same class as the WARM cold-start finding): the cost is real but bounded and
only visible when a program does nothing but spawn empty tasks.

## Gate 17 — spec/tw ≥ 2× floor still holds (in-process, in-binary)

`cargo test --release --test vm_bench -- --ignored --nocapture` (runs/bench = 7, median).
RESIL touched the five async spawn sites and the method-dispatch ladder, so the floor surviving
is the proof those changes did not erode the hot path.

```
compute-bound spec/tw geomean = 5.32x   (Gate 17 floor >= 2.0x)  [PASS]
  every COMPUTE-bound bench still >= 2.0x (min 3.4x, numeric loop)
```

The async-spawn workload, measured in-process across the three engines:

```
benchmark              kind          tw (ms)    gen (ms)   spec (ms)   spec/tw
async spawn (100k)     spawn/alloc   1441.076   1408.712   1404.417    1.03x   (spec/gen ~1.00x)
```

spec ≈ gen ≈ tw — the VM's spawn path (with the capture) shows **no regression vs the generic
VM** in-process. (This is an in-binary comparison; it cannot isolate RESIL-on vs RESIL-off — that
is what Gate 16's cross-binary number does.)

The async-spawn workload is deliberately kept OUT of the shared `benches()` corpus (it lives in
`resil_spawn_benches()`): it is spawn/await-bound and escalates to the async driver at every
`await`, so the LANE/DECODE/DBG per-bench gates (which assume compute-bound workloads the sync
lane can burst) would trip their 1.03× no-regression bound on escalation noise with no payoff.
The RESIL section measures it on its own.

Composition with the other engine gates (unchanged by RESIL):
- DBG armed/none geomean = **1.008×** (PASS — RESIL composes cleanly with the DBG seam).
- LANE no-regression = PASS (every compute bench ≤ 1.03× lane-on/off).
- DECODE on/off geomean = 1.004× (unchanged).
- Overall `vm_vs_treewalker_baseline`: **test result ok, 1 passed**.

## Gate 18 — RSS (no regression)

`/usr/bin/time -l` peak RSS on the 1M-spawn workload:

| | pre-RESIL `main` | branch | ratio |
|---|---|---|---|
| max resident set | ~12.57 MB | ~12.71 MB | **1.011×** |

Flat — the task-local cell holds at most one `Rc<TaskLocals>` (two `Option` fields), and the
spawn-site scope is a stack guard, not a heap allocation.

## Method

- `main` is the exact pre-RESIL baseline (RESIL branched cleanly off `11a5d7d`; `main` did not
  move). Both binaries built `--release` on the same machine in the same session; binaries copied
  aside (`/tmp/ascript-{main,branch}`) so the checkout+rebuild did not clobber the comparison.
- Wall/CPU/RSS via `/usr/bin/time -l`, 5 runs, first discarded as warm-up, median reported.
- The in-process gate is the standing `tests/vm_bench.rs` harness (`#[ignore]`, release).
