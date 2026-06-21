# Record/Replay Benchmark (REPLAY §9, Gates 16-18)

**Date:** 2026-06-20 22:08 UTC
**Host:** Apple M4 (10 logical cores)
**OS:** Darwin 25.5.0 arm64
**Branch binary:** `target/release/ascript` (commit `2f4aedef`)
**Merge-base binary:** `/tmp/ascript_mergebase` (present: yes)
**Rounds per measurement:** 7 (values are medians)

---

## Methodology (same-session protocol, Gate 16)

All numbers were produced in ONE session on the host above. Both binaries are
release builds (`cargo build --release`). The merge-base binary is the pre-REPLAY
`main` HEAD (the commit this branch forks from) — built by `git checkout <merge-base>
&& cargo build --release && cp target/release/ascript /tmp/ascript_mergebase`, then the
branch was checked back out and rebuilt. **No second cargo `target/` is created**
(the disk-safety rule); the merge-base binary is a single copied artifact.

Wall time + peak RSS are read from `/usr/bin/time -l` (macOS; centisecond `real`,
`maximum resident set size`). Each measurement is interleaved across rounds (plain /
record / replay or branch / base in the same loop body) so scheduling noise is shared.
Workloads are deterministic with fixed iteration counts (`bench/replay/*.as`).

**Output parity** (PASS = identical stdout across plain/record/replay, or branch/base)
is the byte-invisibility check — a wrong replay or a divergent off-path would flip it.

---

## (i) Zero-cost-when-off (Gate 12/17)

`bench/replay/zero_cost.as` — a 3 000 000-iter loop that calls `math.*` / `string.*`
builtins every iteration, run PLAIN (no `--record`/`--replay`). The ONLY off-path cost
REPLAY adds is the `trace_active()` `Cell<bool>` read at the top of `call_stdlib` /
`call_native_method` (mirroring the caps `all_granted()` short-circuit). Branch (with
the Cell) vs merge-base (without it):

| Workload | Merge-base (ms) | Branch (ms) | Branch/Base | Output parity |
|----------|-----------------|-------------|-------------|---------------|
| zero_cost (stdlib-heavy loop) | 7820 | 7680 | **0.982x** | PASS |

A `Branch/Base` ratio within run-to-run noise (≈1.0×) confirms the `trace_active()`
read is free on the default path — the Cell is in the right home (the caps
`all_granted()` precedent). The AUTHORITATIVE corpus-wide proof is the standing
`vm_bench` geomean below (REPLAY touches `call_stdlib`, the call path's neighbor).

---

## (ii) Record overhead (Gate 18)

`bench/replay/effect_heavy.as` — 2000 iterations, each doing `fs.write` + `fs.read` +
`math.random` + `time.now` = 8000 recorded/seamed effect calls. PLAIN vs `--record`:

| | Plain (ms) | Record (ms) | Record overhead | Per effect-call | Trace size | Plain RSS | Record RSS |
|---|-----------|-------------|-----------------|-----------------|------------|-----------|------------|
| effect_heavy | 100 | 120 | +20 ms (+20.0%) | 2.50 us | 353,562 B (345 KB) | 13,856 KB | 15,728 KB |

- **Per-effect-call overhead** = (record − plain) / 8000 effect calls. This is the
  cost of appending one event to the in-memory trace buffer per recorded/seamed call.
  Note record can be *faster* than plain here because `math.random`/`time.now` route
  through the SP9 seam under record (no syscall), partly offsetting the append cost —
  so this number is a conservative net, not a pure append cost.
- **Trace size** is the on-disk serialized event log for one full run.
- **Record RSS** vs **Plain RSS** is the in-memory event-buffer cost (Gate 18 — the
  buffer grows with effect count; watch that it stays bounded for the workload size).

---

## (iii) Replay speed

Replay re-runs the program but every recorded effect (fs/process/http) returns its
captured value with NO real OS work, and seamed time/RNG replay from the trace.

### effect_heavy — replay skips all real disk I/O

| | Record (ms) | Replay (ms) | Replay speedup | Replay RSS | Output parity |
|---|------------|-------------|----------------|------------|---------------|
| effect_heavy | 120 | 10 | **12.0x** | 15,520 KB | PASS |

Replay does no `fs.write`/`fs.read` syscalls — the recorded bytes are returned in
memory — so replay collapses to compute + trace-read time.

### sleep_heavy — the SP9 virtual-clock finding

`bench/replay/sleep_heavy.as` — 25 × `time.sleep(20)` = 500 ms of WALL sleep when run
PLAIN. Under BOTH `--record` and `--replay` the clock is the SP9 **virtual** clock, so
`time.sleep` advances virtual time INSTANTLY (no real wall sleep) in either mode:

| | Plain (ms) | Record (ms) | Replay (ms) | Plain→Record | Record→Replay | Parity (rec==rep) |
|---|-----------|-------------|-------------|--------------|---------------|-------------------|
| sleep_heavy | 560 | 10 | 0 | **56.0x** | inf | PASS |

**Honest framing:** the dramatic sleep speedup is **plain → record** (real sleeps become
virtual the moment a determinism context is installed), and record ≈ replay for the sleep
component (both virtual). The script prints `time.monotonic` elapsed: under PLAIN that is
the REAL wall clock (≈566 ms incl. overhead); under record/replay it is the VIRTUAL clock
(exactly the summed sleeps, e.g. 500.0). So plain's stdout differs from record/replay BY
DESIGN (the SP9 virtual-clock seam) — the parity that proves byte-invisibility is
**record == replay** (both virtual), shown PASS above.

### proc_heavy — replay skips fork/exec

`bench/replay/proc_heavy.as` — 30 × `process.run("echo", ...)` (Recorded-Plain).

| | Plain (ms) | Record (ms) | Replay (ms) | Replay speedup | Trace size | Output parity |
|---|-----------|-------------|-------------|----------------|------------|---------------|
| proc_heavy | 50 | 60 | 0 | **inf** | 4,780 B | PASS |

Replay returns the recorded `{stdout,stderr,code}` with NO fork/exec, collapsing the
OS process-spawn cost. (A `0 ms` / `inf` replay column means replay finished below the
`/usr/bin/time` centisecond granularity — process startup dominates, the recorded work
is free.)

---

## vm_bench standing gates (Gate 17)

Re-run after REPLAY (it touches `call_stdlib`, the call path's neighbor):
`cargo test --release --test vm_bench -- --ignored --nocapture` (497.76 s,
`1 passed; 0 failed`). The standing geomean floor + the `dbg_zero_cost_gate`
(instrument==None ≈ armed-idle) — values transcribed from that run:

| Gate | Result | Threshold | Verdict |
|------|--------|-----------|---------|
| main bench spec/tw geomean | **3.78×** (7/9 benches ≥ 2.0×) | ≥ 2.0× | PASS |
| compute-bound spec/tw geomean | **5.17×** | ≥ 2.0× | PASS |
| spec/gen per-bench (sample) | 1.01×–1.97× (specialized ≥ generic) | ≥ 1.0× | PASS |
| `dbg_zero_cost_gate` (armed/none) | **0.969×** (armed-idle within noise of not-attached) | ≈ 1.0× | PASS |

The `dbg_zero_cost_gate` (`0.969×` — armed-idle marginally faster, pure noise) is the
in-binary corroboration of the same zero-cost posture the cross-binary table in §(i)
measures for REPLAY's `trace_active()` `Cell`.

---

## Summary

- **Zero-cost when off:** the default (no-flag) path is within noise of the pre-REPLAY
  binary — the `trace_active()` `Cell` is a free short-circuit (Gate 12/17).
- **Record overhead:** small per-effect-call cost (in-memory event append); trace size
  and record-RSS scale with effect count and stay bounded (Gate 18).
- **Replay speed:** replay skips all real OS effects — dramatically faster for I/O- and
  process-bound workloads; sleeps are virtual under record AND replay (the SP9 seam).
- **Output parity PASS** across every mode is the byte-invisibility proof (the
  corpus-wide cross-engine proof is `tests/record_replay.rs` + `tests/vm_differential.rs`).

*Generated by `bench/run_replay_bench.sh`. Every number traces to the script; no number
is promised in the spec.*
