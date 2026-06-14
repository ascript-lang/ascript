# DECODE — results (Task 11: bench, threshold A/B, same-session A/Bs, the unit verdicts)

This is the DECODE effort's final evidence report. The headline outcome is an
**evidence-gated double drop**: Unit C (speculative global-fn inlining) and Unit D
(TOS register cache) are BOTH reverted on their own A/B data — neither cleared its
ship gate. The decode machinery itself (Units A pre-decode + B superinstruction
fusion) ships, but on the realistic-workload corpus it lands net-neutral-to-slightly-
negative; it is kept for its **invalidation contract** (the JIT prerequisite — see
`goal-perf.md`), not for a measured end-to-end speedup on this corpus.

> Honest single digits were the stated goal. A near-zero / negative result is a
> legitimate, documented outcome — recorded here, never silent.

## Machine / date / methodology

- **Date:** 2026-06-14 (UTC)
- **Host:** Darwin 25.5.0 arm64 — Apple M4
- **Toolchain:** rustc 1.96.0, `--release`
- **Method (disk-safe, no worktree, no second `target/`):** ONE candidate binary
  (`target/release/ascript`), all A/Bs are **env-toggle** on that SAME binary via the
  shipped kill switches (`ASCRIPT_NO_DECODE`, `ASCRIPT_NO_DECODE_INLINE`,
  `ASCRIPT_NO_DECODE_TOS`, `ASCRIPT_DECODE_THRESHOLD`). `bench/ab.sh <base> <cand> 7`
  interleaves base/cand run-by-run (same thermal state); base/cand here are two tiny
  wrappers that `exec` the same binary with a different env var. 7 runs/median per
  workload over the 8-workload profiling corpus. Peak RSS via `/usr/bin/time -l`.
- **`ab.sh` `speedup` column = base_ms / cand_ms** (>1 ⇒ cand faster than base).

## Gate 12/17 + DBG zero-cost (`cargo test --release --test vm_bench -- --ignored`)

Recorded over three release runs (machine noise shifts the absolute numbers run to
run; the clean post-revert run that passed every gate end-to-end — `vm_bench` exit 0 —
is the headline row).

| gate | result | bound | verdict |
|---|---|---|---|
| spec/tw geomean | **3.41–4.00×** (clean run **4.00×**) | ≥ 2.0× | **PASS** |
| every compute-bound bench ≥ 2.0× spec/tw | yes (7/9 benches; 2 alloc-bound exempt) | — | **PASS** |
| `dbg_zero_cost_gate` armed/none | **0.898–0.998×** geomean (clean run **0.998×**) | ≤ 1.05× | **PASS** |
| `decode_on_off` geomean (Units A+B, microbench) | **1.007×** (clean run; sanity gate ≤ 1.05×) | — | **REPORTED** (net-neutral) |

The `decode_on_off` section (new in this task: `Engine::NoDecodeVm` → `vm_run_source_no_decode`)
is REPORTED, not a hard per-bench panic — the microbench per-bench ratios swing ±5–8%
on this single-machine harness (the same noise that makes the LANE per-bench gate
intermittently trip). It asserts only a generous **geomean** sanity bound (≤ 1.05×),
which the clean run clears at 1.007×. The authoritative per-workload Units-A+B verdict
is the realistic `ab.sh` A/B below (0.977×), not these compute kernels.

> Per spec §6.6 the armed-idle config (a profiler/debugger armed) makes any decoded
> stream that contains **inline segments** decline to run (a profiler must see real
> callee frames) — i.e. the armed-idle path loses *only* the inline-fused segments and
> falls back to the byte driver there. With Unit C dropped (below) there are no inline
> segments, so this caveat is moot post-revert; the armed/none ratio above is already
> well under bound regardless (the decoded record stream is itself reachable armed).
> The `armed/none < 1.0` numbers reflect machine noise on the dispatch-light benches,
> not a real armed-faster effect — the gate is the no-*regression* (≤1.05×) bound,
> which holds with margin.

**LANE-section note (pre-existing, not DECODE):** on this busy session the LANE
no-regression gate (`lane_on_off_overhead`, 1.03× bound) trips intermittently on the
dispatch-light / alloc-bound benches (`while loop`, `method dispatch`, `template build`
at 1.03–1.06×) — machine noise against a tight 3% bound, unrelated to DECODE. The
`decode_on_off` section was reordered to run **before** the LANE gate so the Units-A+B
table is always emitted even when the LANE assert aborts the harness on a busy host.

## Threshold A/B (spec §2.3) — pin `DECODE_THRESHOLD`

Same binary, `ASCRIPT_DECODE_THRESHOLD` = 0 / 8 / 32, 7 runs/median, 8-workload corpus.

| comparison (base → cand) | geomean speedup | reading |
|---|---|---|
| thr 8 → thr 0 (decode everything eagerly) | **1.001×** | within noise |
| thr 8 → thr 32 (decode only very hot) | **0.999×** | within noise |

All three thresholds are **statistically identical** on this corpus (max per-workload
delta ≈ ±0.9%, all within run-to-run noise). **Verdict: keep the shipped
`DECODE_THRESHOLD = 8`** (`src/vm/run.rs`) — no winner emerged, so the placeholder
stands per the plan ("if 8 is already best/within-noise, keep it"). Constant unchanged.

## Same-session isolating A/Bs (Gate 16) — the verdict inputs

All four are `bench/ab.sh <base-env-wrapper> <cand-env-wrapper> 7` on the ONE binary.

### A/B 1 — Units A+B contribution (`ASCRIPT_NO_DECODE=1` base vs default cand)

| workload | no_decode ms (base) | default ms (cand) | base/cand | baseMB | candMB |
|---|---:|---:|---:|---:|---:|
| async_inline     | 5253 | 5314 | 0.988× | 12 | 12 |
| async_concurrent | 3145 | 3143 | 1.001× | 12 | 13 |
| json_roundtrip   | 2638 | 2688 | 0.981× | 13 | 13 |
| object_churn     | 2284 | 2351 | 0.971× | 12 | 12 |
| workflow_loop    | 27167 | 27174 | 1.000× | 13 | 13 |
| func_pipeline    | 1094 | 1172 | 0.933× | 13 | 14 |
| call_heavy       | 1084 | 1088 | 0.997× | 12 | 12 |
| server_request   | 1826 | 1921 | 0.950× | 13 | 13 |
| **geomean** | | | **0.977×** | | |

**Reading:** decode-ON (default) is ~**2.3% SLOWER** than decode-OFF on the realistic
corpus; `func_pipeline` (−6.7%) and `server_request` (−5.0%) are the worst, beyond the
0.97 noise bound. The decode warm-up + frame-entry validity check cost is not repaid
by the flatter record stream on these end-to-end workloads (the microbench harness
shows tighter-loop gains, but they don't survive to whole-program time here). **Peak
RSS is flat across all workloads (12–14 MB both arms) — no RSS regression (Gate 18).**

> This is the load-bearing Units-A+B number. It is honest and slightly negative on
> this corpus. DECODE ships for its invalidation-contract value (the JIT prerequisite),
> with this end-to-end result recorded as-is.

### A/B 2 — Unit C contribution (`ASCRIPT_NO_DECODE_INLINE=1` base vs default cand)

| workload | no_inline ms (base) | default ms (cand) | base/cand | baseMB | candMB |
|---|---:|---:|---:|---:|---:|
| async_inline     | 5262 | 5279 | 0.997× | 12 | 12 |
| async_concurrent | 3142 | 3143 | 0.999× | 13 | 13 |
| json_roundtrip   | 2705 | 2708 | 0.999× | 13 | 13 |
| object_churn     | 2288 | 2349 | 0.974× | 12 | 12 |
| workflow_loop    | 24892 | 25032 | 0.994× | 13 | 13 |
| func_pipeline    | 1259 | 1258 | 1.000× | 13 | 13 |
| call_heavy       | 1161 | 1152 | 1.008× | 12 | 12 |
| server_request   | 2068 | 2059 | 1.004× | 13 | 13 |
| **geomean** | | | **0.997×** | | |

**Isolated inline win (default vs no_inline, the call-heavy corpus):**
`func_pipeline` +0.1%, `call_heavy` +0.8% — geomean of the two ≈ **+0.45%**; overall
8-workload inline win ≈ **+0.3%** (`object_churn` is actually −2.7% with inline on).

### A/B 3 — Unit D contribution (`ASCRIPT_NO_DECODE_TOS=1` base vs default cand)

| workload | no_tos ms (base) | default ms (cand) | base/cand | baseMB | candMB |
|---|---:|---:|---:|---:|---:|
| async_inline     | 5279 | 5331 | 0.990× | 12 | 12 |
| async_concurrent | 3153 | 3156 | 0.999× | 13 | 13 |
| json_roundtrip   | 2690 | 2726 | 0.987× | 13 | 13 |
| object_churn     | 2287 | 2360 | 0.969× | 12 | 12 |
| workflow_loop    | 24995 | 23169 | 1.079× | 13 | 13 |
| func_pipeline    | 1237 | 1259 | 0.983× | 14 | 13 |
| call_heavy       | 1157 | 1156 | 1.001× | 12 | 12 |
| server_request   | 2041 | 2059 | 0.991× | 13 | 13 |
| **geomean** | | | **0.999×** | | |

(The `workflow_loop` 1.079× is fsync-dominated — `F_FULLFSYNC` is 96% of that
workload per the Phase-0 profile — so it is pure I/O noise, not a TOS effect.)

**Isolated TOS win on the dispatch-bound trio (default vs no_tos, `object_churn` +
`call_heavy` + `func_pipeline`):** object_churn −3.2% (2360 vs 2287, **regresses past
the 0.97 bound**), func_pipeline −1.8%, call_heavy +0.1% → **trio geomean ≈ −1.6%**
(TOS makes the trio *slower*). RSS flat (12–14 MB), no regression.

### Profiler dogfooding (Gate 16)

`ascript run --profile cpu` on `object_churn` and `call_heavy` (shipped profiler,
speedscope + collapsed formats) both ran cleanly. `object_churn` is a single
top-level loop frame (no sub-calls ⇒ empty function-level sample set, as expected for
a flat hot loop). `call_heavy` collapsed stacks attribute time across the small global
fns `step`/`scale`/`add` (the exact shapes Unit C's speculative inlining targeted) —
confirming dispatch *through the call boundary* is the cost, yet the A/B shows inlining
those fns yields ~0% (the inline+guard machinery's cost cancels the saved dispatch on
this corpus).

## THE UNIT-C VERDICT (spec §6.7) — **DROP**

> *"if the isolated inline win is **< 2% geomean on the call-heavy corpus**, Unit C is
> DROPPED: revert the Task-9 feature commits (keep the deps machinery + its tests —
> they are §4's)…"*

Isolated inline win = **+0.45%** on the call-heavy corpus (`func_pipeline` +0.1%,
`call_heavy` +0.8%), **+0.3%** over the full 8-workload corpus. **< 2% ⇒ DROP.**
Reverted Task-9 feature commit `bd95cd7` (the inline predicate / decode transform /
inline-`Call` arms / loop-back-edge re-decode / §6.6 armed-decline). **KEPT** the
deps-epoch invalidation machinery and its byte-patch battery (Unit A §4 — the JIT
prerequisite), which `bd95cd7` did NOT introduce.

- **Revert commit:** see "Reverts" below.

## THE UNIT-D VERDICT (spec §7.5) — **RECORD-REJECT**

> *"SHIP iff the isolated tos-on win is **≥ 2% geomean on the dispatch-bound trio**
> AND no workload anywhere regresses beyond the 0.97× noise bound. RECORD-REJECT
> otherwise…"*

Isolated TOS win on the dispatch-bound trio = **−1.6% geomean** (object_churn −3.2%,
func_pipeline −1.8%, call_heavy +0.1%) — NOT ≥ 2%, and `object_churn` **regresses to
0.969× (beyond the 0.97 bound).** Both ship conditions fail. **RECORD-REJECT.**
Reverted Task-10 feature commit `4611291` (the `TosCache` accessor layer reverts to
direct fiber ops; the flush-edge battery is deleted with the feature). The
`stack_ops`/`tos_ops` census counters STAY as the recorded evidence (see the residual-
stack-traffic section below — the input that justified *attempting* Unit D at depth).

- **Revert commit:** see "Reverts" below.

### Why the residual-traffic input did not translate into a win

Task 8 measured a real post-fusion residual `stack/decoded` of **> 1.2** (object_churn)
and **~1.5** (func_pipeline) — there genuinely was per-record stack traffic for a TOS
cache to target (the §7.3 gate input below). But eliminating a push/pop pair is cheap
relative to (a) the per-edge flush bookkeeping the correctness contract requires at
EVERY §7.2 exit edge and (b) the burst-local `Option<Value>` accessor indirection on
the hot path. On this M4 the net is a wash-to-loss — the saved stack op is already a
register move the compiler/CPU handles well, while the flush-edge `Option` checks add
real branches. The residual-traffic *share* was a necessary but not sufficient signal;
the A/B is the arbiter, and it says no.

## Unit B (Task 8) — superinstruction fusion: residual stack-traffic share (§7.3)

The post-fusion residual `stack_ops / decoded_ops` for the dispatch-bound trio —
the **Unit D (Task 10) gate input**. Measured via
`tests/vm_decode.rs::decode_residual_stack_traffic_share`
(`cargo test --release --test vm_decode … -- --ignored --nocapture`), forced decode
(threshold 0). The "FUSION OFF" column is a local one-off run with
`FUSION_CANDIDATES` temporarily emptied (NOT a shipped switch) — the delta shows how
much dispatch + stack traffic fusion already removed.

| workload      | decoded_ops (fused) | fused_ops | stack_ops (fused) | stack/decoded (fused) | decoded_ops (FUSION OFF) | stack/decoded (OFF) | dispatch reduction |
|---------------|--------------------:|----------:|------------------:|----------------------:|-------------------------:|--------------------:|-------------------:|
| object_churn  |         162,000,027 | 48,000,001|       198,000,029 |                 1.222 |              228,000,028 |               1.316 |              −29.0% |
| call_heavy    |                  40 |         2 |                44 |                 1.100 |                       42 |               1.095 |              −4.8% (tiny corpus; warmup-bound) |
| func_pipeline |          36,924,041 | 13,756,002|        55,716,043 |                 1.509 |               53,038,043 |               1.443 |              −30.4% |

Fusion removes ~29–30% of dispatches on the two big dispatch-bound workloads and a
chunk of their stack traffic. The residual `stack/decoded` stays > 1.2 / ~1.5 — real
residual traffic existed for a TOS cache to target, which is why Unit D was attempted
at depth rather than fast-tracked to reject. The A/B (above) is the final arbiter:
the residual existed but eliminating it did not pay on this corpus → RECORD-REJECT.

## Reverts (branch left green)

- **Unit D `4611291` reverted FIRST** (it was HEAD, sat atop Unit C and rewrote the
  same `run.rs` regions) → **clean revert, commit `2065217`** (7 files, −758/+163, the
  exact inverse: the `TosCache` accessor reverts to direct fiber ops, the flush-edge
  battery deleted with the feature). The `stack_ops`/`tos_ops` census counters were part
  of the kept measurement plumbing.
- **Unit C `bd95cd7` reverted next** → **clean revert, commit `6fa54d3`** (7 files,
  −1430/+72, the exact inverse: the inline predicate / decode transform / inline-`Call`
  arms / loop-back-edge re-decode / §6.6 armed-decline). Because Unit D was reverted
  first, this applied with **zero conflicts**. **VERIFIED KEPT** (Unit A §4's, the JIT
  prerequisite — bd95cd7 did NOT introduce them): `Chunk::patch_epoch`, `Decoded::own_epoch`
  + the foreign-proto `deps` epoch-vector consult in `src/vm/decode.rs`, and the byte-patch
  invalidation battery in `tests/vm_decode.rs` (`raw_code_patch_byte_has_no_callers_outside_chunk_rs`,
  `cross_module_panic_provenance_survives_the_hoisted_source_refresh`, the on/off byte-identity
  set). The now-inert `decode_inline`/`decode_tos` `Vm` cfg flags + their
  `vm_run_source_decoded_no_inline`/`vm_run_source_decoded_no_tos` harness knobs stay
  (Task-2 plumbing, no-op post-revert).

### Post-revert gates (branch green)

| gate | result |
|---|---|
| `cargo build --release` | clean |
| `cargo clippy --all-targets` (default) | **0 warnings** |
| `cargo clippy --no-default-features --all-targets` | **0 warnings** |
| `cargo test --test vm_differential` (default) | **444 / 0** |
| `cargo test --no-default-features --test vm_differential` | **444 / 0** |
| `cargo test --release --test vm_decode` (kept battery) | **12 / 0** |
| `cargo test --release --test property` (fuzz axis) | **27 / 0** |
| full suite `cargo test --release` (default) | green (0 FAILED) |
| full suite `cargo test --no-default-features --release` | green (0 FAILED) |
| `ASO_FORMAT_VERSION` | **28** (unchanged) |

## JIT-gate verdict (the mandatory `goal-perf.md` re-rank checkpoint)

Does dispatch still dominate after DECODE? **Yes on the tight-loop microbenches, but
the end-to-end A/B shows dispatch is NOT the whole-program bottleneck on the realistic
corpus** — the Phase-0 ranking holds: `async_*` is reactor/park-bound (~70%+),
`json_roundtrip` allocation/hashing-bound, `workflow_loop` fsync-bound (96%),
`object_churn`/`call_heavy`/`func_pipeline` dispatch-bound but already within a small
constant of generic. DECODE's pre-decode + fusion machinery did NOT move whole-program
time on this corpus (Units A+B 0.977×), and the two speculative units (inline, TOS)
each measured below their ship gate. **The JIT precondition DECODE was meant to satisfy
is the invalidation contract (byte-patch → drop decoded code), which the kept deps-epoch
machinery + battery prove — that contract ships; the dispatch *speedup* the JIT would
build on did not materialize from interpretation-level pre-decode here.** The JIT
decision remains evidence-gated downstream (`goal-perf.md`).
