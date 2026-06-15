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
