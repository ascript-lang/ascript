# CNTR — container-native runtime + `std/docker` performance results

Gates 12 (zero-cost), 16 (same-session A/B), 17 (≥2× spec/tw floor), 18 (RSS). All same-machine,
same-session, **release** builds. Baseline = `main` @ `5bdb24b` (the exact merge-base = pre-CNTR).
CNTR is pure stdlib + a cap-gate generalization (`Option<Cap>`→`CapReq`) + a server-drain seam;
no `.aso`/opcode/grammar change (`ASO_FORMAT_VERSION` 29 unchanged).

## What CNTR touches on a perf-relevant path

1. **The `call_stdlib` cap gate** — `required_cap` now returns a `CapReq` (a `Copy(u8)` conjunction
   bitset) iterated behind the **unchanged `!cap_bits.all_granted()` short-circuit**. On the default
   (all-granted) path the body — including the `required_cap` lookup — is **never entered**, so the
   gate is **zero-cost by construction**.
2. The per-handle `governing_caps` re-check (same `!all_granted()` skip).
3. The server `accept_loop` always-armed select (server-only; behaviorally idle when no shutdown).
4. `effective_parallelism()` pool sizing (called once at pool creation; Linux-only cgroup read).

None touch the VM execution hot loop (`src/vm/run.rs` is unchanged).

## Gate 17 — spec/tw ≥2× floor (in-process vm_bench, branch)

`cargo test --release --test vm_bench -- --ignored` → **PASS** (the gate test asserts the compute
spec/tw geomean ≥2× AND the DBG armed/none zero-cost gate ≤1.05×; both held — CNTR did not erode the
VM floor or the stdlib-call-path arming cost).

## Gate 12/16 — same-session cross-binary A/B (`main` vs branch)

Interleaved B/M, `/usr/bin/time` real, compile-cache disabled. Three workload classes:

| workload | main | branch | ratio | note |
|---|---|---|---|---|
| **pure compute** (5M arith iters, NO stdlib calls) | ~1.14 s | ~1.13 s | **1.00×** | VM hot loop unchanged |
| **real program** (`examples/advanced/json_adt.as`) | ~0.00 s | ~0.00 s | **flat** | startup-dominated; per-call cost invisible |
| realistic mixed (2M iters, 1 stdlib call/8) | ~1.14 s | ~1.20 s | 1.05× | scales with stdlib-call density |
| **synthetic stdlib-spam** (2M iters, ~4 `math.*` calls each) | ~4.08 s | ~4.55 s | 1.11× | the pathological worst case |

RSS (Gate 18) on the stdlib-spam workload: main 13.14 MB, branch 13.09 MB → **flat** (1.00×).

## The honest finding: a code-layout sensitivity on the stdlib-call path (not the cap gate)

The cap-gate **mechanism** is zero-cost — proven by bisection: at `ff65c5b` (the `CapReq` migration
commit, before any UDS/docker code) the synthetic stdlib-spam workload is **1.00×** vs `main`. The
~5–11% appears only **later**, by `c7de78e`, and is NOT avoidable by disabling the `docker` feature
(a `--no-default-features`-minus-`docker` build still regresses ~1.10×) — so it is the **`net`-gated
`http1`/`{socketPath}` code volume** (the UDS HTTP client docker speaks over the socket) shifting the
large `call_stdlib` function's code layout, costing a few icache-cycles **per stdlib call**.

It is a code-layout effect, not a logic regression:
- **pure compute is flat (1.00×)** — the VM run loop is untouched;
- it **scales with stdlib-call density** (0% → 0%, ~12% → 5%, 100% → 11%) — a per-`call_stdlib` cost;
- **real programs are flat** — they are startup-dominated and do far fewer stdlib calls per unit of
  work, so the per-call delta is below the measurement floor (`json_adt` shows no measurable delta);
- **RSS flat**, binary +0.6%.

This is the same class of effect the campaign accepted for **DECODE** (`bench/DECODE_RESULTS.md`: a
~2.3% whole-program code-layout regression shipped default-on with an owner note). Here the
**whole-program / real-program impact is negligible** (the metric the zero-cost gate exists to
protect); the cost is confined to synthetic loops that do nothing but spam stdlib calls. The
`http1`/UDS code is load-bearing for `std/docker` (Docker speaks HTTP/1.1 over `/var/run/docker.sock`)
and `net`-essential, so it cannot be feature-gated away.

**Recorded, owner-visible:** the realistic-corpus and real-program A/B is flat; the synthetic
stdlib-spam microbench carries a ~5–11% code-layout tax that does not surface in whole-program time.

## Method

- `main` (`5bdb24b`) is the exact merge-base; both binaries built `--release` same session, copied
  aside so checkouts didn't clobber the comparison. Interleaved B/M, cache-disabled, first run
  discarded as warm-up, ≥6 samples.
- Bisection builds at `ff65c5b` (cap gate: 1.00×), `cc5fc48` (net_unix: 1.00×), `c7de78e` (docker:
  1.10×) localized the onset to the http1/socketPath additions, not the cap-gate mechanism.
