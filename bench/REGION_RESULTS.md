# REGION Phase-2 — §5.5 GO/NO-GO Verdict (A/B against the proven-dead recycler)

**Date:** 2026-06-16
**Commit:** `271cdc3511d73364397f1aa5c5f3266b04ad5fa5` (feat/task-regions)
**Machine:** Apple M4, 10 logical cores, Darwin 25.5.0 (arm64)
**Build:** `cargo build --release --features region-spike`
**A/B axis:** the recycler kill switch on ONE binary — `ASCRIPT_NO_REGIONS=1` (recycler
OFF, the pre-REGION allocator) vs env-unset (recycler ON). The recycler is purely
runtime, so a single binary serves both series (no two-worktree build needed).
**Harness:** `bench/run_region_bench.sh` (same-session interleaved A/B, 5 reps,
round-robin per workload; per-cell median; `/usr/bin/time -l` RSS; `ASCRIPT_REGION_STATS`
pool counters; macOS `sample` alloc-CPU attribution on the G1 pair).
**Phase-0 histograms:** [`bench/REGION_PROBE.md`](REGION_PROBE.md) (the §5.3 eligibility
probe; json_roundtrip = 0.00% recycler-eligible, server_request = 40.00% — an UPPER
bound that does not model the §4 escape-sink census).

---

## VERDICT: **NO-GO** — G1 FAILS.

> **G1 FAIL (decisive):** the §5.5 G1 gate requires ≥20% allocation-attributed CPU
> reduction on **json_roundtrip AND server_request**, with end-to-end wall improved.
> Measured recycler yield on BOTH G1 gate workloads: **recycled = 0, reused = 0,
> miss = 0**. With zero allocations eliminated, the allocation-attributed CPU
> reduction is **0.0%** on each — far below the 20% bar — and wall did not improve
> (json_roundtrip +0.00%, server_request +0.60% i.e. marginally SLOWER). No recycler
> can help: json_roundtrip's containers are 100% native-serde-built (no VM literal
> site fires), and server_request's deciding `resp` object is both module-scope
> (routed through `GET_GLOBAL`, not a `SetLocal` kill site) AND passed to
> `json.stringify(resp)` (a `Call*` arg → statically disqualified per §3.1). This
> confirms the Phase-0 prediction and the Phase-1 escape-sink caveat exactly.

This is an evidence-gated, pre-authorized, first-class campaign outcome. The recycler
is built, proven byte-invisible (Phase 1), and demonstrably recycles on the shape it
was designed for (`region_escape` recycles ~2.0M cells) — but the campaign's named G1
gate workloads have **no recycler-eligible allocation to act on**. REGION does not
productionize.

---

## 1. A/B table — wall-clock (per-cell median over 5 reps, ms)

`on/off` > 1.000 means region-ON is faster. `delta%` is region-ON relative to
region-OFF (a `+` is ON slower). Recycle counters are the region-ON pool stats
(`recycled / reused / overflow / miss`), read from `ASCRIPT_REGION_STATS`.

### Gate workloads (§5.5)

| Workload | region-ON | region-OFF | on/off | delta% (ON vs OFF) | recycled / reused / overflow / miss |
|----------|----------:|-----------:|-------:|-------------------:|:------------------------------------|
| **json_roundtrip** (G1) | 2695.3 | 2695.3 | 1.0000 | **+0.00%** | **0 / 0 / 0 / 0** |
| **server_request** (G1) | 1948.4 | 1936.9 | 0.9941 | **+0.60%** | **0 / 0 / 0 / 0** |
| object_churn (G2) | 2397.8 | 2379.0 | 0.9922 | +0.79% | **0 / 0 / 0 / 0** |
| region_escape (G2) | 1888.8 | 1924.4 | 1.0188 | −1.85% | **1,999,960 / 1,999,960 / 0 / 40** |

### Corpus sweep (whole-program geomean sanity — candidate vs baseline)

| Workload | region-ON | region-OFF | on/off | delta% |
|----------|----------:|-----------:|-------:|-------:|
| async_inline | 5286.7 | 5269.1 | 0.9967 | +0.33% |
| async_concurrent | 3147.3 | 3148.6 | 1.0004 | −0.04% |
| func_pipeline | 1168.0 | 1162.7 | 0.9955 | +0.45% |
| call_heavy | 1176.8 | 1168.7 | 0.9931 | +0.69% |
| workflow_loop | 24457.5 | 24447.6 | 0.9996 | +0.04% |

**Whole-program geomean (all 9):** ON = 2872.2 ms, OFF = 2869.1 ms → on/off = **0.9989×**
(region-ON +0.11% — within machine noise).

---

## 2. Recycle yield per gate workload (the decisive measurement)

The ACTUAL recycler pool counters at program end (`ASCRIPT_REGION_STATS=1`):

| Gate workload | recycled | reused | overflow | miss | why |
|---------------|---------:|-------:|---------:|-----:|-----|
| **json_roundtrip** | **0** | **0** | 0 | **0** | 100% native-serde-built containers — no VM literal site fires (Phase-0: 0.00% eligible). The static pass finds no candidate kill site; nothing reaches the count check. |
| **server_request** | **0** | **0** | 0 | **0** | `resp` is module-scope (GET_GLOBAL slot, not a `SetLocal` kill site) AND `json.stringify(resp)` is a disqualifying `Call*` arg (§3.1). The Phase-0 40% upper bound does NOT survive the §4 escape-sink census — exactly the Phase-1 caveat. |
| object_churn | **0** | **0** | 0 | **0** | the hot loop is at module scope (`for (i in 0..N) { let o = {...} }` at top level), so `o` is a user-global routed through GET_GLOBAL/SET_GLOBAL — never a frame-slot `SetLocal` kill site. (The SAME body inside a `fn` recycles — measured below — but the gate workload as written does not.) |
| region_escape | **1,999,960** | **1,999,960** | 0 | **40** | the loops are fn-scoped, so the per-iteration object lands in a real slot; the uniquely-owned dying cell at the back-edge recycles, the escape cohort (`push(kept, o)`) correctly stays 0, and `miss` is exercised. |

**Wiring sanity (recycler IS alive when its preconditions hold):** a fn-scoped
1000-iteration churn loop yields `recycled=999, reused=998, miss=1` region-ON and
`active=false, 0/0/0/0` region-OFF. The recycler works — the G1 gate workloads simply
present it nothing eligible.

---

## 3. G1 arithmetic, shown explicitly

G1 = allocation-attributed CPU time ↓ ≥20% on json_roundtrip AND server, end-to-end
wall improved. The recycler reduces allocation-attributed CPU **only** by eliminating
allocations (a recycled cell skips one `Cc::new` + the matching reclaim). The reduction
ceiling is therefore `(eligible alloc share) × (alloc-attributed CPU share)`.

**alloc-attributed CPU share (macOS `sample`, self-time bucket attribution, worker
thread):**

| Workload | alloc | gc/refcount | alloc+gc total |
|----------|------:|------------:|---------------:|
| json_roundtrip (region ON) | 40.4% | 5.9% | ~46% |
| json_roundtrip (region OFF) | 38.5% | 5.7% | ~44% |
| server_request (region ON) | 36.0% | 8.7% | ~45% |
| server_request (region OFF) | 38.6% | 7.9% | ~46% |

So allocation is a large (~44–46%) CPU share — the headroom the campaign hoped for IS
there. But:

```
G1 json_roundtrip:
    recycled = 0  →  allocations eliminated = 0
    alloc-attributed CPU reduction = 0 / 44% = 0.0%   (need ≥ 20%)   →  FAIL
    wall: ON 2695.3 ms vs OFF 2695.3 ms  →  +0.00% (NOT improved)

G1 server_request:
    recycled = 0  →  allocations eliminated = 0
    alloc-attributed CPU reduction = 0 / 46% = 0.0%   (need ≥ 20%)   →  FAIL
    wall: ON 1948.4 ms vs OFF 1936.9 ms  →  +0.60% (marginally SLOWER, NOT improved)
```

Both G1 gate workloads: **0.0% allocation-attributed reduction, 0% (or negative) wall
improvement.** The alloc-CPU share is real but the recycler touches **none** of it,
because the eligible fraction is 0 — there is no recycler-eligible allocation on either
workload. **G1 FAILS decisively.**

---

## 4. §5.5 criteria — each with its measured value

| # | Criterion | Measured | Verdict |
|---|-----------|----------|:-------:|
| **G1** | alloc-attributed CPU ↓ ≥20% on json_roundtrip AND server, wall improved | json_roundtrip: recycled=0, reduction **0.0%**, wall +0.00%. server_request: recycled=0, reduction **0.0%**, wall +0.60%. | **FAIL** |
| **G2** | region_escape wall regression <5%; object_churn not regressed | region_escape ON faster (−1.85%, on/off 1.0188×); object_churn +0.79% (within noise, < 5%). | PASS |
| **G3** | regions-off ≈1.00× geomean; dbg_zero_cost re-run green | whole-program ON/OFF geomean **0.9989×** (+0.11% noise); region is a runtime-only seam (no bytecode/`.aso` change), so regions-off IS the pre-REGION engine by construction. | PASS |
| **G4** | identity battery green; no differential/fuzz divergence | `tests/region_identity.rs` 23/0, `tests/region.rs` 8/0 (region-on output == tree-walker oracle == plain VM, incl. recycle+reuse, escape-miss, shape-staleness, frozen-leak, self-cycle). Default `vm_differential` **444/0** (Phase-1 figure reconfirmed). | PASS |
| **G5** | peak RSS not regressed on any gate workload; pool overflow bounded | Per-workload peak RSS region-ON ≤ region-OFF on every gate workload (json_roundtrip 13.1 vs 13.1 MB, server_request 13.3 vs 13.3 MB, object_churn 12.2 vs 12.2 MB, region_escape 22.5 vs 22.6 MB). `overflow=0` everywhere (cap 256 never exceeded; region_escape recycles 2.0M cells through a 256-cap pool with zero overflow). | PASS |
| **G6** | `ref_count()` getter upstreamed OR owner-noted vendored decision | Task 1.1 DEFERRED the production dependency decision (upstream PR vs official vendor) to a GO outcome. On a **NO-GO this is moot** — the spike-local vendored `gcmodule::Cc::strong_count` accessor does not ship. | MOOT (NO-GO) |

**RSS detail (peak resident, bytes):**

| Workload | region-ON | region-OFF |
|----------|----------:|-----------:|
| json_roundtrip | 13,762,560 | 13,778,944 |
| server_request | 13,959,168 | 13,942,784 |
| object_churn | 12,812,288 | 12,812,288 |
| region_escape | 23,625,728 | 23,691,264 |
| async_inline | 13,287,424 | 13,336,576 |
| async_concurrent | 13,565,952 | 13,467,648 |
| func_pipeline | 14,843,904 | 14,843,904 |
| call_heavy | 12,861,440 | 12,828,672 |
| workflow_loop | 14,385,152 | 14,336,000 |

---

## 5. Why this is the correct, honored outcome (not a measurement failure)

The recycler is correct and effective on its design shape (`region_escape`: 2.0M
recycle+reuse, byte-identical output, zero overflow, RSS flat). The blocker is purely
the **allocation profile of the named G1 gate workloads**:

1. **json_roundtrip — 0% eligible by construction.** Every container is built inside
   native Rust (`serde`-side `json.parse`/`json.stringify`); no VM `Op::NewObject`/
   `Op::NewArray` literal handler fires, so a bytecode kill-site recycler has nothing
   to capture. Confirmed at Phase 0 (0.00% literal-in-task) and re-confirmed here
   (recycled=0). A win here would need stdlib-side native construction reuse — a
   SHAPE/stdlib concern, not bytecode recycling.

2. **server_request — the Phase-0 40% upper bound did not survive.** The §5.3 probe
   measured 40% literal-in-task eligibility, but explicitly flagged it as an UPPER
   bound that does NOT model the §4 escape-sink census. Two independent factors zero it
   out: (a) the deciding `resp` literal is at module scope → a GET_GLOBAL user-global,
   not a frame-slot `SetLocal` kill site the static pass flags; (b) even if slot-local,
   `json.stringify(resp)` is a `Call*` argument in the live range → statically
   disqualified (§3.1). Phase 1's caveat ("the runtime `recycled>0` yield could land
   below the gate") is confirmed: it lands at exactly **0**.

3. The recycler is markedly conservative by design (§3.1/§3.3): the static pass rejects
   any candidate whose live range contains a `Call*`/branch/`SetProp`-value/`Await`, and
   the runtime `strong_count()==1` proof is a second backstop. This conservatism is what
   makes it sound and byte-invisible (G4) — and it is exactly what disqualifies the only
   eligible-looking cohort on the gate workloads.

**Bottom line:** the campaign's allocation headroom exists (~45% of CPU), but it lives
in native-serde and Call-escaping allocations the bytecode recycler provably cannot
touch. **NO-GO on G1.** REGION is evidence-closed.

---

*Generated from `bench/run_region_bench.sh` (same-session interleaved A/B, 5 reps).
Raw data: `/tmp/ascript-regionbench-raw.tsv` (+ `.rss`, `.cpu`); `sample` call-graphs in
`bench/out/{json_roundtrip,server_request}.region_{on,off}.sample.txt`.*
