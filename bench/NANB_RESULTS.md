# NANB Benchmark Results

NaN-boxing campaign (`feat/nan-boxing`) — per-phase gate records.

---

## Phase 1 — Sealed `Value` struct (zero-cost seam)

**Task 1.8 — Phase-1 zero-cost proof (Gate 12/17 + DBG)**
**Date:** 2026-06-15 | **Machine:** Apple M4 | **Profile:** release (7 runs/bench, median)

### What was measured

Phase 1 is a PURE REFACTOR: `Value` is now `pub struct Value(ValueRepr)` over a private
`enum ValueRepr`; the `ValueKind`/`OwnedKind` view layer inlines away completely.
`size_of::<Value>()` is UNCHANGED at **24 bytes** (same layout, just renamed and wrapped).
`ASO_FORMAT_VERSION` is UNCHANGED at **28** (no opcode or serialization change).

The zero-cost claim: geomean vs main's pre-NANB DECODE Task-11 baseline (4.00×) is ≈ 1.00×.

### Benchmark table

```
benchmark                  kind     spec/tw   spec/gen
fib(30) recursion          compute    8.60x     1.28x
sum recursion (500 x2000)  compute    9.14x     1.28x
numeric loop (1e6)         compute    3.72x     1.07x
while loop (1e6)           compute    5.99x     1.23x
property r/w (1e6)         compute    4.15x     1.09x
method dispatch (1e6)      compute    4.10x     2.08x
string concat (50000)      alloc      1.40x     1.11x   (EXEMPT from >= 2x)
template build (50000)     alloc      1.18x     1.03x   (EXEMPT from >= 2x)
closure capture (1e6)      compute    6.27x     1.23x

geomean spec/tw = 4.07x   (pre-NANB DECODE baseline 4.00x — within noise)
```

### Gate verdicts

| Gate | Result | Value |
|------|--------|-------|
| Compute-bound >= 2x spec/tw (Gate 12/17) | **PASS** | all 7, min 3.72x |
| No spec-vs-generic regression (>= 0.97x) | **PASS** | every bench |
| DBG zero-cost gate armed/none geomean | **PASS** | 1.005x (<= 1.05x) |
| size_of::<Value>() unchanged at 24 bytes | **PASS** | 24 |
| ASO_FORMAT_VERSION unchanged at 28 | **PASS** | 28 |
| Clippy --all-targets (default features) | **PASS** | 0 warnings/errors |
| Clippy --all-targets --no-default-features | **PASS** | 0 warnings/errors |

### DBG zero-cost gate detail

```
benchmark                  none (ms)  armed (ms)  armed/none
fib(30) recursion           322.464    324.999      1.008x
sum recursion (500 x2000)   130.320    129.929      0.997x
numeric loop (1e6)          100.765    100.669      0.999x
while loop (1e6)            129.417    131.024      1.012x
property r/w (1e6)          179.792    180.200      1.002x
method dispatch (1e6)       292.203    289.216      0.990x
string concat (50000)        49.934     51.744      1.036x
template build (50000)       78.242     77.494      0.990x
closure capture (1e6)       322.321    325.166      1.009x

geomean armed/none = 1.005x  [PASS <= 1.05x]
```

The dispatch-arm text was touched by the NANB seam migration, so the DBG re-run rule
applied (CLAUDE.md: "dispatch-arm text was touched by the migration → the DBG re-run rule
applies"). The gate holds.

### Conclusion

The Phase-1 seam (sealed `Value` struct + `ValueKind`/`OwnedKind` view layer) is
**ZERO-COST**: geomean spec/tw 4.07× matches the pre-NANB baseline 4.00× within run-to-run
noise. The enum-view-inlines-away claim is verified empirically. Phase 2 may proceed.

---

## Phase 3 — the evidence: cross-repr differential, deep fuzz, same-session A/B

**Date:** 2026-06-15 16:03 UTC | **Machine:** Apple M4 (10 cores) / Darwin 25.5.0 arm64 | **Commit:** 1b77ba2 | **Reps:** 7 (interleaved, per-cell median)

Same-session A/B: the 24-byte default repr vs the 16-byte `--features value16` repr, two RELEASE binaries built by feature-toggle into ONE `target/` (no worktree). `size_of::<Value>()` = **24** (default) vs **16** (value16), asserted by each binary's own `value_size` test. Speedup = base_ms / v16_ms (>1.000 means value16 is FASTER).

### Time A/B — per workload, per VM mode

| workload | base-spec ms | v16-spec ms | spec× | base-gen ms | v16-gen ms | gen× | base-tw ms | v16-tw ms | tw× |
|---|---|---|---|---|---|---|---|---|---|
| async_inline | 5245 | 5225 | 1.004× | 5323 | 5330 | 0.999× | 5678 | 5782 | 0.982× |
| async_concurrent | 3136 | 3165 | 0.991× | 3148 | 3152 | 0.999× | 3963 | 3949 | 1.004× |
| json_roundtrip | 2703 | 2737 | 0.988× | 2736 | 2766 | 0.989× | 3465 | 3484 | 0.995× |
| object_churn | 2310 | 2346 | 0.985× | 4592 | 4598 | 0.999× | 11660 | 11427 | 1.020× |
| workflow_loop | 24835 | 25021 | 0.993× | 24476 | 24802 | 0.987× | 24698 | 25103 | 0.984× |
| func_pipeline | 1130 | 1070 | 1.056× | 2926 | 2825 | 1.036× | 6501 | 6469 | 1.005× |
| call_heavy | 1143 | 1120 | 1.020× | 1540 | 1483 | 1.039× | 8738 | 8682 | 1.006× |
| server_request | 1951 | 1942 | 1.005× | 2165 | 2177 | 0.994× | 3940 | 3909 | 1.008× |

**Geomean speedup (value16 / default):**  spec **1.005×**  ·  gen **1.005×**  ·  tree-walker **1.000×**

### Peak RSS A/B (Gate 18) — spec mode

| workload | base RSS (MB) | v16 RSS (MB) | Δ (v16-base) | v16/base |
|---|---|---|---|---|
| async_inline | 12.5 | 12.7 | +0.2 | 1.015 |
| async_concurrent | 13.0 | 12.9 | -0.0 | 0.998 |
| json_roundtrip | 13.0 | 13.1 | +0.1 | 1.006 |
| object_churn | 12.1 | 12.2 | +0.0 | 1.004 |
| workflow_loop | 13.7 | 13.6 | -0.0 | 0.998 |
| func_pipeline | 14.2 | 14.0 | -0.2 | 0.986 |
| call_heavy | 12.2 | 12.3 | +0.2 | 1.014 |
| server_request | 13.2 | 13.1 | -0.1 | 0.992 |

**Geomean RSS ratio (value16 / default) = 1.001×**  (< 1.000 means value16 uses LESS memory — the 24→16 byte case).

### Correctness evidence (Tasks 3.1 / 3.2)

**Task 3.1 — cross-BINARY old-vs-new repr differential** (`scripts/nanb-cross-repr-diff.sh`).
The within-process `vm_differential` cannot prove the repr is behavior-invisible on its own
(both engines in ONE binary share the repr — spec §0). Two separately-built RELEASE binaries
(24-byte default, 16-byte `--features value16`) from the SAME commit, run over the WHOLE
examples corpus, stdout/stderr/exit-code diffed byte-for-byte:

```
corpus files : 127
ran (diffed) : 110
skipped      :  17   (nondeterministic / server / relative-import — the EXAMPLE_SKIPS mirror)
DIFFS        :   0
RESULT       : byte-identical across both reprs (110/110)
```

Plus the full four-mode in-process differential under the new repr — both feature configs:

```
cargo test --features value16 --test vm_differential                       -> 444 passed, 0 failed
cargo test --no-default-features --features value16 --test vm_differential  -> 444 passed, 0 failed
```

**Task 3.2 — deep fuzz campaign** (the FUZZ ~284k bar set by merge `9b202eb`).

```
FUZZ_STRESS_N=300000 cargo test --release --features value16 --test property \
  stress_differential_many_seeds -- --ignored --nocapture
-> 300,000 generated programs, EACH through 8 engine modes (tree-walker, specialized-VM,
   generic-VM, .aso round-trip, lane-off, no-call-fast, decoded-forced, no-decode) — all
   on the 16-byte repr (the cross-repr axis). 0 divergences. (finished in 1190 s)
```

> **Campaign finding (fixed in-branch, failing-test-first — Gate 0):** the FIRST 300k run
> crashed with `Multiplication overflowed` from `rust_decimal` propagated as
> `worker thread panicked`. Root cause is **REPR-INDEPENDENT** — it reproduces
> byte-identically on BOTH the 24-byte and 16-byte binaries (a pre-existing decimal bug
> surfaced by the higher case count, NOT a value16 regression): `apply_binop` and the VM
> `decimal_fast` path used bare `+ - * / %` on `Decimal`, which `panic!` on overflow.
> Fixed to checked ops raising a recoverable Tier-2 `decimal <op> overflowed` from the
> shared site (commit `1b77ba2`); the full 300k campaign then completed clean from scratch.

### Gate 12/17 re-run under `value16` (`cargo test --release --features value16 --test vm_bench`)

| Gate | Result | Value |
|------|--------|-------|
| spec/tw geomean ≥ 2× (compute-bound, Gate 12/17) | **PASS** | **4.03×** (7/9 ≥ 2.0×; every compute-bound ≥ 2.0×) |
| DBG zero-cost (armed-idle / none ≤ 1.05×) | **PASS** | **0.996×** |
| LANE on/off no-regression | **PASS** | 0.777× (lane faster) |

Profiled (shipped profiler, observation-only, stdout byte-identical) — `call_heavy.as` under
value16 produced a clean function-level call tree (`<script>;step;scale;add` …, collapsed format),
confirming `--profile cpu` works under the new repr. (`json_roundtrip` profiles empty because its
hot loop is almost entirely inside native `json.stringify`/`parse`, not script frames.)

### Honest reading — verdict INPUTS (the SHIP/REJECT call is Phase 4's, not this task's)

Measured against spec §8.1's criteria (recorded, not judged here):

- **Time A/B is a WASH.** Geomean spec **1.005×**, gen **1.005×**, tw **1.000×** — all inside
  run-to-run noise. Per-workload it is mixed: the call-bound `func_pipeline` (+5.6% spec) and
  `call_heavy` (+2.0% spec, +3.9% gen) modestly favor value16; the alloc/decode-bound
  `object_churn` (−1.5%), `json_roundtrip` (−1.2%), and `async_concurrent` (−0.9%) modestly
  favor the 24-byte default. No workload is outside ±2% in spec mode. There is no clear
  speed WIN.
- **RSS A/B shows NO meaningful memory win.** Geomean v16/base = **1.001×** — flat. The 33%
  shrink of each `Value` cell (24→16 B) does NOT move peak RSS on these workloads because
  their peak is dominated by the ~12–14 MB runtime image and native buffers, not the live
  `Value` array/stack footprint. A memory WIN would need a Value-array-dominated working-set
  workload (very large arrays/objects of scalars); the profiling corpus does not contain one,
  so the headline memory case for the 16-byte repr is **NOT demonstrated** by this data.
- **Correctness is fully GREEN:** cross-binary 110/110 byte-identical, four-mode 444/0 (both
  configs), 300k-case deep fuzz 0-divergence, Gate-12 spec/tw 4.03× and DBG 0.996× under
  value16. ThinStr stays Miri-clean (Phase 2). ASO_FORMAT_VERSION unchanged at 28.

**Reading:** the data LOOKS like a **WASH / lean-REJECT** against §8.1 — value16 is correct and
behavior-invisible, but it is neither faster (geomean ~1.005× spec, inside noise; alloc-bound
workloads slightly slower) NOR meaningfully lighter (RSS 1.001×, flat) on the measured corpus.
This mirrors the prior 16-byte attempt's evidence-REJECT (`COMPACT_VALUE_RESULTS.md`). The
SHIP/REJECT verdict is Phase 4's to render against the fixed §8.1 criteria — this section only
supplies the honest numbers.

---

## Phase 4 — the verdict: STOP (evidence-rejected)

**Date:** 2026-06-15 | **Reviewer-of-record:** independent (not the implementer) | **Branch:** `feat/value16` @ `1ad210f` | **Rendered strictly against spec §8.1 (criteria fixed BEFORE measurement, non-negotiable).**

### Verdict against each §8.1 criterion

| # | Criterion (§8.1) | Bar | Measured | Verdict |
|---|---|---|---|---|
| 1 | Geomean time | spec ≥ **1.02×** AND gen ≥ 1.00× | spec **1.005×** · gen 1.005× | **FAIL** — spec misses 1.02× by ~1.5 pp; rides the noise band the spec explicitly forbids riding |
| 2 | No pathological regression + STRING subset | no HOT < 0.97× either mode; STRING geomean ≥ 0.99× both | min spec 0.985× (object_churn), none < 0.97×; **STRING subset geomean NOT isolated/reported** | **FAIL** — the named VAL failure mode (STRING) was not measured on the §8.2 string corpus, so the ≥0.99× sub-gate is unconfirmable |
| 3 | Memory: peak RSS ≥ **5%** median improvement on the data-heavy set | ≥ 5% | RSS geomean **1.001×** (flat) | **FAIL** — 0% vs required ≥5%; the 24→16 B shrink is swamped by the ~12–14 MB runtime image + native buffers |
| 4 | Tree-walker geomean within 0.97× | ≥ 0.97× | **1.000×** | **PASS** |
| 5 | All §7 correctness green + clippy/tests both configs | all green | cross-binary 110/110 byte-identical · four-mode 444/0 ×2 configs · 300k-case deep fuzz 0 divergence · Gate-12 spec/tw 4.03× · DBG 0.996× · ASO_FORMAT_VERSION 28 unchanged · ThinStr Miri-clean | **PASS** |

### Overall: **STOP — evidence-rejected (3 of 5 criteria fail).**

### Diagnosis

Criteria 1 and 3 are the decisive misses, with criterion 2 unconfirmable. The time A/B is a
**wash**: specialized geomean **1.005×** against a hard **≥1.02×** bar — inside the run-to-run
noise band §8.1 forbids riding ("the win must clear the measured noise band, not ride it"). The
memory case is a clean miss: peak RSS geomean **1.001× (flat)** versus the required **≥5% median
improvement** — the 33% shrink of each `Value` cell does not move peak RSS on the profiling
corpus, whose peak is dominated by the runtime image and native buffers, not the live `Value`
working set. (A memory win would need a `Vec<Value>`-of-scalars-dominated working set; the corpus
does not contain one, so the headline memory case is **not demonstrated**.) The STRING-subset
geomean — the exact thin-`Str` failure mode VAL named — was never isolated on the §8.2 string
corpus, so its ≥0.99× sub-gate cannot be confirmed either. Correctness (4, 5) is fully green and
`value16` is provably behavior-invisible — but per §0/§8.1 the repr ships ONLY on a measured win,
and there is none. This mirrors the prior 16-byte thin-`Str` evidence-reject
(`bench/COMPACT_VALUE_RESULTS.md`) exactly.

### Disposition (PATH B — RECORD-REJECT)

- The `value16` repr (Phases 2–3: `ThinStr`, the `cfg(value16)` 16-byte two-word `Value`) is **NOT
  merged.** It stays frozen on `feat/value16`, flagged, as the cheap re-run path if hardware or a
  future SSO/thin-`Str` variant changes the calculus.
- **Phase 1's seam** (sealed `pub struct Value(ValueRepr)` + `ValueKind`/`OwnedKind` view) **remains
  merged on `main`** — the permanent hygiene win, proven zero-cost (geomean 4.07× == baseline 4.00×).
- The **repr-INDEPENDENT decimal-overflow fix** (`apply_binop` / VM `decimal_fast` → checked ops
  raising a recoverable Tier-2 `decimal <op> overflowed`, found by the 300k fuzz campaign, fixed
  failing-test-first) **lands on `main`** — it is a real bug fix, not part of the rejected repr.
- `goal-perf.md` NANB row → **evidence-rejected**; the **JIT** precondition 2 (≤16 B value) remains
  **UNMET at 24 B** (JIT stays deferred unless its own re-profile overrides); **REGION**'s "value
  representation final" dependency is satisfied at **24 B**, so REGION is unblocked.
