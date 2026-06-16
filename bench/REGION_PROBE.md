# REGION Phase-0 Probe — §5.3 Checkpoint Verdict

**Date:** 2026-06-16  
**Commit:** e1693730fe606f214dbbb1277fba64bdfda28816 (feat/task-regions)  
**Machine:** Apple M4, macOS 26.5.1 (arm64)  
**Build:** `cargo build --release --features region-probe`  
**Probe env:** `ASCRIPT_REGION_PROBE_OUT=<path>`

---

## The §5.3 checkpoint criterion (verbatim)

> **Proceed to Phase 1 only if** the region-eligible share — allocations born at a *bytecode
> literal site* that die within their birth task — is **≥ 25% of allocation events on at least
> one gate workload** (`json_roundtrip` or the server workload). Below that, even a perfect
> recycler cannot reach the ≥20% allocation-time gate (allocation is ≤ 38% of total time; the
> recycler can only touch the eligible fraction of it). If the share is high only on
> `object_churn` (likely — it is literal-shaped by construction), that is recorded but does
> NOT satisfy the checkpoint: the campaign gate names `json_roundtrip` + the server workload.
> A checkpoint failure is a full NO-GO, recorded with the histogram (§5.6).

**Numerator:** allocations born at `Op::NewObject`/`Op::NewArray` (`SiteClass::Literal`) whose
birth task is still live at drop time (`died_in_task = true`).  
**Denominator:** all allocation events tracked by the probe (all five container kinds ×
{Literal, Native} × {in-task, escaped}).

---

## Probe histogram results

### `bench/profiling/json_roundtrip.as` (1 run, 700,000 iterations)

Raw output:
```json
{"object":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":1400000,"escaped":0}},
 "array":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":2800000,"escaped":0}},
 "map":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "set":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "instance":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}}}
```

| Kind     | Literal in-task | Literal escaped | Native in-task | Native escaped |
|----------|----------------:|----------------:|---------------:|---------------:|
| object   |               0 |               0 |      1,400,000 |              0 |
| array    |               0 |               0 |      2,800,000 |              0 |
| map      |               0 |               0 |              0 |              0 |
| set      |               0 |               0 |              0 |              0 |
| instance |               0 |               0 |              0 |              0 |
| **TOTAL**|           **0** |           **0** |  **4,200,000** |          **0** |

**Checkpoint arithmetic:**
- Numerator (literal in-task): **0**
- Denominator (all alloc events): **4,200,000**
- Share = 0 / 4,200,000 = **0.00%**
- Threshold: 25%
- **§5.3 verdict for `json_roundtrip`: FAIL**

**Interpretation:** exactly as predicted by spec §1.2 — `json_roundtrip`'s allocations are
entirely dominated by `json.parse`/`json.stringify` building `Value` trees inside native Rust
(`serde`-side); no object or array is constructed at a VM bytecode literal site. A bytecode
escape analysis has zero eligible allocation to work with here.

---

### `bench/profiling/server_request.as` (1 run, 500,000 request iterations)

This is the LANE Task-0 server workload (`bench/LANE_RESULTS.md`, `bench/run_call_bench.sh`):
a run-to-completion request-handler simulation — parse JSON, route, construct response object,
stringify. No sockets; deterministic.

Raw output:
```json
{"object":{"literal":{"in_task":1000000,"escaped":0},"native":{"in_task":500000,"escaped":0}},
 "array":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":1000000,"escaped":0}},
 "map":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "set":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "instance":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}}}
```

| Kind     | Literal in-task | Literal escaped | Native in-task | Native escaped |
|----------|----------------:|----------------:|---------------:|---------------:|
| object   |       1,000,000 |               0 |        500,000 |              0 |
| array    |               0 |               0 |      1,000,000 |              0 |
| map      |               0 |               0 |              0 |              0 |
| set      |               0 |               0 |              0 |              0 |
| instance |               0 |               0 |              0 |              0 |
| **TOTAL**|   **1,000,000** |           **0** |  **1,500,000** |          **0** |

**Checkpoint arithmetic:**
- Numerator (literal in-task): **1,000,000**
- Denominator (all alloc events): **2,500,000**
- Share = 1,000,000 / 2,500,000 = **40.00%**
- Threshold: 25%
- **§5.3 verdict for server workload: PASS**

**Interpretation:** each of the 500,000 iterations constructs exactly 2 response-body objects
at VM literal sites (`{status: 200, body: {…}}`), both dying within the main "task" scope
(task 0 = main, which never retires before program end — spec §5.2). The native allocations
are 1 `json.parse` result object + 1 `json.stringify` source traversal array per iteration.
The 40% eligible share is in the addressable range for a recycler.

---

### `bench/profiling/object_churn.as` (1 run, 2,000,000 iterations)

Raw output:
```json
{"object":{"literal":{"in_task":6000000,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "array":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "map":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "set":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}},
 "instance":{"literal":{"in_task":0,"escaped":0},"native":{"in_task":0,"escaped":0}}}
```

| Kind     | Literal in-task | Literal escaped | Native in-task | Native escaped |
|----------|----------------:|----------------:|---------------:|---------------:|
| object   |       6,000,000 |               0 |              0 |              0 |
| **TOTAL**|   **6,000,000** |           **0** |          **0** |          **0** |

**Share: 6,000,000 / 6,000,000 = 100.00%**

Recorded per spec §5.3: "If the share is high only on `object_churn` (likely — it is
literal-shaped by construction), that is recorded but does NOT satisfy the checkpoint."
This result is expected and does not contribute to the GO decision.

---

### Example corpus (aggregate, 72 program runs)

72 probe-file lines accumulated from `examples/*.as` (skipping long-running/interactive:
`server_multicore`, `workers_*`, `caps_sandbox`, `ffi_*`, `bundle_*`).

| | Literal in-task | Literal escaped | Native in-task | Native escaped |
|---|---:|---:|---:|---:|
| All kinds | 983 | 3 | 1,178 | 5 |
| **TOTAL** | | | | **2,169** |

- Literal in-task share: 983 / 2,169 = **45.3%**
- These are micro-counts (toy programs); not a gate workload.

---

## §5.3 Checkpoint verdict

| Gate workload | Literal in-task | Total alloc events | Share | ≥ 25%? |
|---|---:|---:|---:|:---:|
| `json_roundtrip` | 0 | 4,200,000 | **0.00%** | NO |
| `server_request` (LANE Task-0) | 1,000,000 | 2,500,000 | **40.00%** | **YES** |
| `object_churn` (non-gate) | 6,000,000 | 6,000,000 | 100.00% | (excluded per spec) |

**CHECKPOINT RESULT: GO to Phase 1.**

The server workload (`server_request.as`) passes at **40.00% ≥ 25% threshold**. The spec
requires "at least one gate workload" — the server workload is the second named gate workload.

**`json_roundtrip` at 0.00%:** the probable cause named in spec §1.2 is confirmed — native
stdlib code (`json.parse`/`stringify` via `serde`) constructs all containers; no VM literal
handler fires. A recycler cannot help `json_roundtrip`. This finding is recorded: any
allocation win on `json_roundtrip` would need stdlib-side improvements (native-side
construction, a SHAPE/stdlib concern), not bytecode recycling.

**What the server workload shows:** the `{status, body}` response object literal per request
is the 1,000,000-count eligible cohort. With a 40% eligible share and allocation at 38% of
CPU time in `json_roundtrip`-class code (§1.2), the theoretical ceiling for a perfect server
recycler is ~15% end-to-end speedup on the server workload. That is above the ≥20%
allocation-time gate (G1) — worth attempting.

**Proceed to Phase 1** (Task 1.1 — gcmodule `ref_count()` getter fork).
