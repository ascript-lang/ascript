# DECODE — results (in progress; Task 11 folds this into the final report)

This file accumulates the DECODE effort's measured numbers as the units land. It
is a STUB — Task 11 (Unit E / the final report) rewrites it into the full headline
(A/B geomean, RSS, `dbg_zero_cost_gate`, etc.).

## Machine / date

- **Date:** 2026-06-14 (UTC)
- **Host:** Darwin 25.5.0 arm64 — Apple M4
- **Toolchain:** rustc 1.96.0, `--release`

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

### Reading this for Unit D (Task 10)

- Fusion removes **~29–30%** of dispatches on the two big dispatch-bound workloads
  (object_churn, func_pipeline) and a chunk of their stack traffic.
- The **post-fusion residual** `stack/decoded` stays **> 1.2** on object_churn and
  **~1.5** on func_pipeline — i.e. each surviving record still averages > 1
  push/pop. There is real residual stack traffic for a TOS register cache (Unit D)
  to target; Unit D is **worth attempting at depth** rather than fast-tracked to a
  RECORD-REJECT verdict. (Task 10 makes the final call against its own design cost.)
- `call_heavy` is a tiny run-to-completion corpus (40 records) dominated by the
  call boundary, not straight-line staging — fusion barely engages there, as
  expected (the census-dominant fused shapes are the arithmetic/field staging
  loops, which object_churn and func_pipeline exercise heavily).

> Note: object_churn's `stack/decoded` *decreased* under fusion (1.316 → 1.222)
> even though `stack_ops` per fused record is currently attributed at the HEAD
> op's magnitude (a lower-bound proxy on the fused record's traffic) — so the
> residual share is, if anything, conservative. The DELTA between the two columns
> is the load-bearing Unit-D input and is measured identically on both sides.
