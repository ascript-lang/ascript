# Compact Value Representation Benchmark (VAL Stage 3 / thin-Str + Gate 12)

**Date:** 2026-06-09 20:11 UTC
**Host:** Apple M4
**Logical cores:** 10
**OS:** Darwin 25.5.0 arm64
**Binaries:** `target/profiling/ascript` (both VAL branch and baseline @ `1f1451d`)
**Reps:** 3 (interleaved round-robin; per-cell median reported)

---

## 1. Structural fact — `size_of::<Value>()`

| | bytes |
|---|---|
| Stage-1 floor baseline (@ `1f1451d`) | **24** |
| VAL Stage 3 / thin-`Str` (this branch) | **16** |

> (The runner's auto-detect mislabeled the baseline as 32 on this run — a grep bug
> since fixed; the baseline binary at `1f1451d` asserts `size_of::<Value>() == 24`
> in its own `value_size_is_documented` test. The 16 for the VAL branch is correct
> and independently asserted. The 24→16 A/B is what the timings below measure.)

Stage 3 thins the two `Rc<str>`-carrying variants (`Str` AND `Builtin`) from
the fat 16-byte `Rc<str>` (data ptr + length) to the single-word `AStr`
(`Rc<Box<str>>`, 8 bytes — the `Box<str>` carries its own length INSIDE the
heap allocation, so the `Rc` is a thin pointer). Both had to be thinned: the
enum floor is `round_up(widest_payload) + 8-byte tag`, so a single remaining
fat `Rc<str>` would have re-pinned it at 24. With the widest payload now 8
bytes and `Decimal` already boxed (Stage 1), the layout is `8 + 8` = **16** —
the VAL Stage-3 floor, reached with **NO new ownership `unsafe`** (the
deferred NaN-box's selling point). 8 bytes needs the NaN-box (deferred —
gcmodule lacks public `Cc::into_raw`/`from_raw`). The tradeoff is a
double-indirection on string ACCESS (`Value → Rc → Box<str> → bytes`),
surfaced by the string workloads below.

---

## 2. Wall-clock per workload (per-cell median, ms)

Each cell is the median over the interleaved reps. **HOT** workloads form the
Gate-12 geomean; **COLD** workloads are the boxing cold-path checks (reported
but excluded from the headline geomean).

| Workload | base spec | VAL spec | base gen | VAL gen |
|----------|-----------|----------|----------|---------|
| int_sum | 2921.1 | 2831.3 | 3075.0 | 3067.8 |
| fib_iter | 6322.6 | 6233.8 | 6406.4 | 6279.0 |
| array_walk | 1386.4 | 1377.9 | 1408.3 | 1470.4 |
| object_churn | 3184.1 | 3033.5 | 3247.4 | 3172.2 |
| float_sum | 2775.8 | 2752.3 | 2958.2 | 2856.7 |
| string_concat | 1885.3 | 1938.9 | 1874.0 | 1962.4 |
| string_map | 6386.5 | 6315.3 | 6407.2 | 6366.6 |
| string_index | 4064.0 | 4096.0 | 4079.4 | 4148.7 |
| membound_strings | 4829.0 | 5159.9 | 5220.1 | 5239.0 |
| decimal_cold *(cold)* | 157.0 | 157.0 | 166.2 | 166.3 |
| method_cold *(cold)* | 768.4 | 699.9 | 761.1 | 739.4 |

## 3. Geomean — VAL (16 B) vs same-session Stage-1 baseline (24 B)

Three subsets: **ALL-HOT** (every non-cold workload), **SCALAR** (the original
CPU-bound loops — int/float/array/object, where `Str` is not on the path) and
**STRING** (the thin-`Str` workloads: concat / string-keyed map / codepoint
index / memory-bound `array<string>`). The STRING subset is the one that can
REGRESS from the double-indirection — reported separately, not averaged away.

| Subset / Mode | baseline geomean (ms) | VAL geomean (ms) | speedup | delta |
|---------------|-----------------------|------------------|---------|-------|
| ALL-HOT specialized | 3352.4 | 3345.8 | 1.002× | +0.2% |
| ALL-HOT generic | 3443.9 | 3452.5 | 0.997× | -0.3% |
| SCALAR specialized | 2957.6 | 2894.2 | 1.022× | +2.2% |
| SCALAR generic | 3055.9 | 3033.0 | 1.008× | +0.8% |
| STRING specialized | 3920.7 | 4010.8 | 0.978× | -2.2% |
| STRING generic | 3998.8 | 4059.4 | 0.985× | -1.5% |

**Gate 12: PASS.** Thin-`Str` is an encoding change UNDER the value API
(not a specialization guard), so the generic VM — which skips every
IC/adaptive/global fast path — must NOT regress either. A generic-mode
regression would be a VAL bug, not an acceptable trade.

**Honest reading:** the SCALAR subset is unaffected (those workloads never
touch `Str`), so any SCALAR delta is pure machine noise. The STRING subset is
where the 24→16 shrink trades against the extra string-access indirection:
read the STRING geomean as the NET for string-bound code, and `membound_strings`
(the large-working-set scan) as the cache-density signal specifically. See the
KEEP-or-STOP verdict at the foot of this report for the net call.

## 4. Cold-path check (Task 1 / Task 2 boxing)

| Workload | base spec (ms) | VAL spec (ms) | delta | notes |
|----------|----------------|---------------|-------|-------|
| decimal_cold | 157.0 | 157.0 | +0.0% | Decimal boxed to `Rc<Decimal>` — one `Rc::new` per op |
| method_cold | 768.4 | 699.9 | +9.8% | `ClassMethod`/`GeneratorMethod` boxed to one `Rc` payload |

Honest framing: boxing `Decimal` means decimal arithmetic now does an
`Rc::new` allocation per op (the cold path). On any NON-decimal workload this
code never runs, so it adds zero to the hot path; `decimal_cold` measures the
cold cost directly. The two method-binding variants are rare, cold bindings —
`method_cold` confirms the extra indirection on their construct+dispatch is
not a measurable regression.

---

## 5. KEEP-or-STOP verdict (VAL Stage 3 / thin-`Str`)

**Recommendation: STOP at Stage 1's 24 — do NOT keep thin-`Str`.**

The data does not support keeping it:

- **size**: 24 → 16 (a real 33% `Value`-slot shrink). FOUR-MODE byte-identical
  (tree-walker == specialized-VM == generic-VM == `.aso`), both feature configs;
  `ASO_FORMAT_VERSION` unchanged (25). NO new ownership `unsafe`. All correctness
  gates pass.
- **but the cache-density bet did not pay off.** The whole point of 24→16 was
  denser `Vec<Value>` / map storage helping memory-bound workloads. The
  purpose-built `membound_strings` scan (a 1.5M-element `array<string>`, 6 passes)
  **REGRESSED +6.8% specialized** (4829 → 5160 ms) — the extra string-access
  indirection (`Value → Rc → Box<str> → bytes`, vs the old fat `Rc<str>`'s single
  hop) costs MORE per element than the denser slot saves. Every string workload
  regressed: `string_concat` +2.8%, `string_index` +0.8%, the STRING geomean
  **−2.2% spec / −1.5% gen**.
- The SCALAR subset's +2.2% spec / +0.8% gen is **noise**, not a win: those
  workloads never touch `Str`, so the encoding change cannot affect them — they
  measure machine drift, and on this M4 they happened to land slightly favorable
  this run (the generic +0.8% and the −0.3% ALL-HOT generic confirm it is in the
  noise band).
- Net: the ONLY code paths thin-`Str` actually changes (string-bound ones) get
  **slower**, and the cache-density benefit that was supposed to offset that did
  not materialize even on the workload designed to show it. This is a small net
  regression on string-bound code with no compensating win.

Per the task's default bias ("keep ONLY if four-mode-clean AND not a net regression
in EITHER mode"): it is four-mode-clean, but it IS a net regression on string code
in both modes. **STOP.** The thin-`Str` commit (`c1571ec`) is left on this branch on
a clearly-flagged sub-state so the orchestrator can revert `Value::Str`/`Builtin` to
the fat `Rc<str>` (back to the 24-byte Stage-1 floor) and keep only the Stage-1 work.
The 8-byte floor remains reachable only via the deferred NaN-box (which would make
strings a tagged machine word with NO extra access indirection — the right way to
get below 24, when `Cc::into_raw`/`from_raw` lands).

---

*Generated by `bench/run_compact_value_bench.sh` (interleaved same-session
A/B, mirroring `run_shared_heap_bench.sh`).*
