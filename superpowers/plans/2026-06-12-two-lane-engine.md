# Two-Lane Engine (LANE) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

**Goal:** Add a synchronous dispatch driver (`run_loop_sync`) over the existing `Fiber` so the
suspension-free opcode subset executes outside the async machinery, with the async `run_loop` as
the orchestrator that handles only genuine suspension points; `await` on an already-resolved
future completes inline. Ship the campaign's Phase-0 bench corpus (Task 0) first so every change
has a before/after number.

**Architecture:** Two drivers over one `Fiber` (`src/vm/fiber.rs` — frames/ip/stack externalize
ALL execution state). `run_loop_sync` is a plain fn returning `SyncOutcome::{Finished(RunOutcome),
NeedsAsync}`; the async `run_loop` bursts into it at the top of each loop iteration and executes
exactly one escalation op per iteration otherwise. Escalation = decode the op, check the §3
subset BEFORE advancing `ip`, return with `ip` untouched. Kill switch `Vm.sync_lane`
(`ASCRIPT_NO_SYNC_LANE=1`) mirrors `--no-specialize`. Spec:
`superpowers/specs/2026-06-12-two-lane-engine-design.md` (read it fully before any task).

**DEFER coordination (owner sequencing — DEFER merges BEFORE LANE):** branch from a base that
contains DEFER (`2026-06-12-defer-statement-design.md`); LANE rebases over its frame-exit/unwind
drain hooks. Classification (spec §3 "DEFER coordination"): `DeferPush`/`DeferPushMethod` are
IN the sync subset (they only push a bound thunk — suspension-free); the `Return`/`Propagate`
arms escalate (`NeedsAsync`, ip un-advanced) when `frame.defers` is non-empty, so the
potentially-awaiting drain always runs on the async driver.

**Tech stack:** Rust (single binary `ascript`); the bytecode VM (`src/vm/run.rs`, `src/vm/fiber.rs`),
the M17 async runtime (`src/task.rs`), tokio current-thread + `LocalSet`. Tests via `cargo test`
(BOTH feature configs), `tests/vm_differential.rs`, `tests/vm_bench.rs`, `fuzz/fuzz_targets/
differential.rs`, `bench/profiling/` + the new `bench/ab.sh`.

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house trailer on
  every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14):** any bug found while working — ours or
  pre-existing, direct or incidental — is fixed **in this branch** with a failing-test-first
  regression guard, never stepped around. No placeholders, no silent deferrals.
- Byte-identity is never relaxed: a lane-on/lane-off/tree-walker divergence is a bug in the sync
  driver or the handoff. Fix the driver, never the assertion.
- Clippy clean AND tests green under `--all-targets` and `--no-default-features --all-targets`
  before any "done" claim. Evidence (command output) before assertions.
- Branch: `feat/two-lane-engine` off `main` **after DEFER has merged** (rebase over its
  frame-exit/unwind hooks if it moves). Merge `--no-ff` after holistic review.

---

## File Structure

**New files:**
- `bench/profiling/func_pipeline.as` — functional-idiom workload (map/filter/reduce).
- `bench/profiling/call_heavy.as` — small-function-call-dominated workload.
- `bench/profiling/server_request.as` — request-shaped glue workload (socket-free, deterministic).
- `bench/ab.sh` — same-session A/B harness (two binaries, interleaved runs, geomean, peak RSS).
- `bench/LANE_RESULTS.md` — the LANE A/B + RSS report (Task 9).

**Modified files:**
- `src/task.rs` — `ResultCell::try_get` / `SharedFuture::try_get`.
- `src/vm/run.rs` — `Vm.sync_lane` + lane counters, `SyncOutcome`, `run_loop_sync`,
  the orchestrator burst in `run_loop`, the `push_closure_frame` extraction.
- `src/lib.rs` — `vm_run_source_no_sync_lane`, `vm_run_source_lane_stats`,
  `ASCRIPT_NO_SYNC_LANE` CLI seam in the `run` path.
- `tests/vm_differential.rs` — lane-mode differential battery + corpus mode + coverage assertion.
- `tests/vm_bench.rs` — `NoSyncLaneVm` engine + lane on/off section; gate re-runs.
- `fuzz/fuzz_targets/differential.rs` + `tests/property.rs` — the lane-off fuzz projection.
- `bench/profiling/run.sh` — new workloads in `BENCHES`.
- `bench/PROFILING_RESULTS.md` — dated Phase-0-extension + post-LANE sections.
- `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md` — status/architecture updates (Task 10).

---

## Task 0: Phase-0 bench corpus + same-session A/B harness (BEFORE any engine change)

**Files:**
- Create: `bench/profiling/func_pipeline.as`, `bench/profiling/call_heavy.as`,
  `bench/profiling/server_request.as`, `bench/ab.sh`
- Modify: `bench/profiling/run.sh`, `bench/PROFILING_RESULTS.md`

- [ ] **Step 1: Write the three workloads** (style mirrors the existing `bench/profiling/*.as`:
  `std/time` monotonic timing, deterministic, prints `name: …=… elapsed_ms=…`, runs to
  completion on both engines). Verify each runs on VM and `--tree-walker` with identical
  non-timing output.

`bench/profiling/func_pipeline.as`:
```ascript
// PROFILE TARGET (PERF campaign blind spot): functional idioms. map/filter/reduce
// pipelines over realistic records — per-element callback re-entry (fresh fiber +
// boxed future per element today), closure dispatch, small-object reads.
import * as time from "std/time"

let records = []
for (i in 0..2000) {
  records = records.concat([{ id: i, score: i % 97, group: i % 7, active: i % 3 != 0 }])
}

let t0 = time.monotonic()
let acc = 0
for (round in 0..200) {
  let total = records
    .filter((r) => r.active && r.score > 10)
    .map((r) => r.score * 2 + r.group)
    .reduce((a, b) => a + b, 0)
  acc = acc + total
}
let t1 = time.monotonic()
print(`func_pipeline: acc=${acc} elapsed_ms=${t1 - t0}`)
```

`bench/profiling/call_heavy.as`:
```ascript
// PROFILE TARGET (PERF campaign blind spot): call-dominated code. Tiny plain
// functions called in a hot loop — measures per-call cost (arg vec, cells vector,
// frame push/pop, contract check) with negligible arithmetic per call.
import * as time from "std/time"

fn add(a, b) { return a + b }
fn scale(x) { return add(x, x) }
fn step(x) { return add(scale(x), 1) }

let t0 = time.monotonic()
let sum = 0
for (i in 0..2000000) {
  sum = add(sum, step(i % 1000))
}
let t1 = time.monotonic()
print(`call_heavy: sum=${sum} elapsed_ms=${t1 - t0}`)
```

`bench/profiling/server_request.as`:
```ascript
// PROFILE TARGET (PERF campaign blind spot): server-request-shaped glue, WITHOUT
// sockets (deterministic, run-to-completion — joins the corpus; real accept-loop
// serving is covered by tests/server_multicore.rs). Per "request": parse a JSON
// body, route via a handler map, build a response object, stringify it.
import * as json from "std/json"
import * as time from "std/time"

fn handle_get(req) { return { status: 200, body: { id: req.id, ok: true } } }
fn handle_put(req) { return { status: 200, body: { id: req.id, saved: req.payload } } }
fn handle_missing(req) { return { status: 404, body: { error: "no route" } } }

let routes = { "GET /item": handle_get, "PUT /item": handle_put }

let t0 = time.monotonic()
let bytes = 0
for (i in 0..150000) {
  let raw = `{"method":"${i % 2 == 0 ? "GET" : "PUT"}","path":"/item","id":${i},"payload":"p${i % 50}"}`
  let [req, e1] = json.parse(raw)
  let key = `${req.method} ${req.path}`
  let handler = routes[key] ?? handle_missing
  let resp = handler(req)
  let [out, e2] = json.stringify(resp)
  bytes = bytes + len(out)
}
let t1 = time.monotonic()
print(`server_request: bytes=${bytes} elapsed_ms=${t1 - t0}`)
```

  (Implementer: verify each program's stdlib usage compiles as written — e.g. `.concat`,
  `routes[key] ?? …`, ternary-in-template — against the real corpus idioms in `examples/**`;
  adjust to the closest supported idiom if any construct differs, keeping the measured shape.
  Tune iteration counts so each lands in the 1.5–6 s range on the dev machine, matching the
  existing workloads.)

- [ ] **Step 2: Write `bench/ab.sh`** — the same-session A/B harness (the SRV MINOR-2 lesson:
  baseline and candidate in ONE invocation on one machine):

```bash
#!/usr/bin/env bash
# Same-session A/B harness (PERF campaign Gate 16). Runs BASELINE and CANDIDATE
# ascript binaries interleaved over the profiling workloads, reports per-workload
# medians, the candidate/baseline speedup, the geomean, and peak RSS (Gate 18).
#   Usage: bench/ab.sh <baseline-binary> <candidate-binary> [runs=5]
set -euo pipefail
cd "$(dirname "$0")/.."
BASE="$1"; CAND="$2"; RUNS="${3:-5}"
BENCHES=(async_inline async_concurrent json_roundtrip object_churn workflow_loop \
         func_pipeline call_heavy server_request)

median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
run_ms() { "$1" run "bench/profiling/$2.as" 2>/dev/null \
            | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2; }
peak_rss_mb() { /usr/bin/time -l "$1" run "bench/profiling/$2.as" 2>&1 >/dev/null \
            | grep -i "maximum resident" | grep -oE '[0-9]+' | head -1 \
            | awk '{printf "%d", $1 / 1048576}'; }

printf "%-16s | %10s | %10s | %8s | %6s | %6s\n" bench "base ms" "cand ms" speedup baseMB candMB
total_ln=0; n=0
for f in "${BENCHES[@]}"; do
  bs=(); cs=()
  for ((r=0; r<RUNS; r++)); do            # interleave: same-session, same thermal state
    bs+=("$(run_ms "$BASE" "$f")"); cs+=("$(run_ms "$CAND" "$f")")
  done
  bm=$(printf '%s\n' "${bs[@]}" | median); cm=$(printf '%s\n' "${cs[@]}" | median)
  sp=$(awk -v b="$bm" -v c="$cm" 'BEGIN {printf "%.3f", b / c}')
  brss=$(peak_rss_mb "$BASE" "$f"); crss=$(peak_rss_mb "$CAND" "$f")
  printf "%-16s | %10.0f | %10.0f | %7sx | %6s | %6s\n" "$f" "$bm" "$cm" "$sp" "$brss" "$crss"
  total_ln=$(awk -v t="$total_ln" -v s="$sp" 'BEGIN {print t + log(s)}'); n=$((n+1))
done
awk -v t="$total_ln" -v n="$n" 'BEGIN {printf "geomean speedup = %.3fx\n", exp(t / n)}'
```

- [ ] **Step 3:** `chmod +x bench/ab.sh`; add the three workloads to `BENCHES` in
  `bench/profiling/run.sh`.
- [ ] **Step 4: Re-baseline.** `cargo build --profile profiling && bench/profiling/run.sh`
  (expect the table to now include the three new rows) and
  `bench/ab.sh target/profiling/ascript target/profiling/ascript` (self-A/B sanity: geomean
  ≈ 1.00x, proving the harness's noise floor). Record both outputs.
- [ ] **Step 5:** Append a dated section to `bench/PROFILING_RESULTS.md`:
  `## Phase-0 extension (2026-06-XX) — functional / call-heavy / server-request workloads`
  with the headline-timings table (VM ms, tree-walker ms, speedup, peak RSS) for the three new
  workloads + the bucket attribution from `parse_sample.py`, and a note that this is the
  pre-LANE baseline every PERF spec A/Bs against.
- [ ] **Step 6: Commit** — `bench: Phase-0 corpus extension (func/call/server workloads) + same-session A/B harness` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer re-runs `run.sh` + the self-A/B, confirms the three
  workloads are deterministic (two runs, identical non-timing output), VM == tree-walker output,
  and the PROFILING_RESULTS section matches the actual numbers.

## Task 1: `SharedFuture::try_get` (the inline-completion primitive)

**Files:**
- Modify: `src/task.rs`
- Test: inline `#[tokio::test]`s in `src/task.rs`

- [ ] **Step 1: Write the failing tests** (in the existing `mod tests`):

```rust
#[tokio::test]
async fn try_get_pending_is_none_resolved_is_some() {
    let f = SharedFuture::new();
    assert!(f.try_get().is_none(), "pending future must probe as None");
    f.resolve(Ok(Value::Float(7.0)));
    assert_eq!(f.try_get().unwrap().unwrap(), Value::Float(7.0));
    // Read-only: probing does not consume — get() and a second try_get still agree.
    assert_eq!(f.get().await.unwrap(), Value::Float(7.0));
    assert_eq!(f.try_get().unwrap().unwrap(), Value::Float(7.0));
}

#[tokio::test]
async fn try_get_carries_stored_control() {
    let f = SharedFuture::new();
    f.resolve(Err(Control::Panic(AsError::new("boom"))));
    match f.try_get().unwrap() {
        Err(Control::Panic(e)) => assert_eq!(e.message, "boom"),
        other => panic!("expected stored panic, got {other:?}"),
    }
}

#[tokio::test]
async fn try_get_never_touches_the_abort_handle() {
    // Probing a pending future must not cancel/detach the backing task: after a
    // try_get, dropping the last handle still aborts (cancel-on-drop unchanged).
    use std::cell::Cell as StdCell;
    let local = tokio::task::LocalSet::new();
    local.run_until(async move {
        let ran = Rc::new(StdCell::new(false));
        let ran2 = ran.clone();
        let f = SharedFuture::new();
        let cell = f.cell();
        let jh = tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            ran2.set(true);
            cell.resolve(Ok(Value::Float(1.0)));
        });
        f.set_abort(jh.abort_handle());
        assert!(f.try_get().is_none()); // probe while pending
        drop(f);                        // last handle -> abort still fires
        for _ in 0..5 { tokio::task::yield_now().await; }
        assert!(!ran.get(), "cancel-on-drop must survive a try_get probe");
    }).await;
}
```

- [ ] **Step 2: Run — expect FAIL** (no `try_get`): `cargo test --lib task::tests`
- [ ] **Step 3: Implement** — on `ResultCell`: `fn try_get(&self) -> Option<Result<Value,
  Control>> { self.0.slot.borrow().as_ref().cloned() }`; on `SharedFuture`:
  `pub fn try_get(&self) -> Option<Result<Value, Control>> { self.0.cell.try_get() }` with a doc
  comment stating the §4 contract (non-blocking, non-consuming, never notifies, never touches
  the abort handle).
- [ ] **Step 4: Run — expect PASS**; `cargo clippy --all-targets` clean.
- [ ] **Step 5: Commit** — `feat(task): SharedFuture::try_get non-blocking resolved-slot probe (LANE §4)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer confirms `try_get` cannot deadlock with `get()`'s borrow
  (scoped borrow, no await), and that no other code path was touched.

## Task 2: `Vm.sync_lane` kill switch + lane counters + entry points (no dispatch change yet)

**Files:**
- Modify: `src/vm/run.rs` (Vm struct + constructors), `src/lib.rs`
- Test: `tests/vm_differential.rs` (new test) + inline unit test in `src/vm/run.rs`

- [ ] **Step 1: Write the failing test** (in `tests/vm_differential.rs`):

```rust
#[tokio::test]
async fn no_sync_lane_entry_point_runs_byte_identically() {
    // The kill switch must exist and (pre-driver) be a pure no-op: identical output.
    let src = "let s = 0\nfor (i in 0..100) { s = s + i }\nprint(s)";
    let on = ascript::vm_run_source(src).await.expect("lane-on ok");
    let off = ascript::vm_run_source_no_sync_lane(src).await.expect("lane-off ok");
    assert_eq!(on, off);
    // Lane stats: before the driver exists, BOTH modes retire 0 sync ops.
    let (_out, _exit, sync_ops, bursts) =
        ascript::vm_run_source_lane_stats(src).await.expect("stats ok");
    assert_eq!((sync_ops, bursts), (0, 0), "no driver yet — counters must read 0");
}
```

- [ ] **Step 2: Run — expect FAIL** (entry points don't exist):
  `cargo test --test vm_differential no_sync_lane_entry_point`
- [ ] **Step 3: Implement:**
  - `Vm` fields (beside `specialize`, `src/vm/run.rs:117`):
    `sync_lane: bool`, `lane_sync_ops: Cell<u64>`, `lane_bursts: Cell<u64>`; accessors
    `pub fn lane_sync_ops(&self) -> u64` / `pub fn lane_bursts(&self) -> u64`.
  - Constructor: `with_specialize` defaults `sync_lane` from the env —
    `sync_lane: std::env::var("ASCRIPT_NO_SYNC_LANE").as_deref() != Ok("1")` — plus a
    `pub fn with_lanes(interp, specialize: bool, sync_lane: bool) -> Rc<Self>` explicit
    constructor for tests (env never read when the explicit form is used). Document that the
    env default is what makes worker isolates inherit the kill switch.
  - `src/lib.rs`: thread a `sync_lane: bool` through `vm_run_source_cfg` (`lib.rs:2269`);
    add `#[doc(hidden)] pub async fn vm_run_source_no_sync_lane(src)` and
    `#[doc(hidden)] pub async fn vm_run_source_lane_stats(src) -> Result<(String, Option<i32>,
    u64, u64), AsError>` (the latter returns `(output, exit, vm.lane_sync_ops(),
    vm.lane_bursts())`). Mention `ASCRIPT_NO_SYNC_LANE` beside the existing
    `ASCRIPT_NO_SPECIALIZE` comment in the CLI `run` path (`lib.rs:2060–2067`).
- [ ] **Step 4: Run — expect PASS.** Then both clippy configs + `cargo test --test
  vm_differential` (full file) — green.
- [ ] **Step 5: Commit** — `feat(vm/lane): sync_lane kill switch + lane counters + test entry points (inert)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer greps that NO dispatch-loop line changed yet (the flag is
  inert), confirms env-var handling matches the `ASCRIPT_NO_SPECIALIZE` precedent, and that
  parallel tests never read the env.

## Task 3: Extract `push_closure_frame` (behavior-preserving refactor of the plain-call body)

**Files:**
- Modify: `src/vm/run.rs` (the `Op::Call` `Value::Closure` plain arm, `run.rs:1757–1827`)
- Test: existing suites are the guard (refactor task — no new behavior)

- [ ] **Step 1: Extract** the plain-closure call body into one shared, plain (non-async) method:

```rust
/// LANE Task 3 (shared by the async `Op::Call` arm and `run_loop_sync`): the
/// plain synchronous closure call. Pops `argc` args + the callee slot, runs the
/// SHARED `check_call_args`, allocates cells, pushes the CallFrame (ONE
/// `enter_frame_depth` — SP3 §B), publishes profiler frames. No await.
fn push_closure_frame(
    &self,
    fiber: &mut Fiber,
    callee: Cc<Closure>,
    argc: usize,
    callee_idx: usize,
    call_span: Span,
) -> Result<(), Control> {
    /* the moved body of run.rs:1757–1827, verbatim */
}
```

  The async `Op::Call` arm's `Value::Closure(callee) => { ... }` plain case becomes a single
  call to this helper. **Nothing else moves** (the async/worker/generator callee cases stay
  inline in the arm).
- [ ] **Step 2: Prove behavior-preserving:** `cargo test --test vm_differential` (both feature
  configs) — green; `cargo test` (full, both configs) — green; clippy both configs — clean.
- [ ] **Step 3: Commit** — `refactor(vm): extract push_closure_frame — the shared plain-call body (LANE Task 3)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer diffs the extracted body against the pre-move arm
  line-by-line (must be verbatim: same `check_call_args` call shape, ONE `enter_frame_depth`,
  `publish_profile_frames` after the push) and re-runs the corpus differential.

## Task 4: `run_loop_sync` skeleton + orchestrator (core subset: consts/stack/arith/locals/jumps)

**Files:**
- Modify: `src/vm/run.rs` (+ `pub(crate) enum SyncOutcome`)
- Test: `tests/vm_differential.rs`

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn sync_lane_executes_the_tight_loop_and_counts_it() {
    // Anti-false-green (LANE §6.4): the lane must actually retire instructions.
    let src = "let s = 0\nfor (i in 0..1000000) { s = s + i }\nprint(s)";
    let (out, _exit, sync_ops, bursts) =
        ascript::vm_run_source_lane_stats(src).await.expect("ok");
    assert_eq!(out, "499999500000\n");
    assert!(sync_ops >= 1_000_000, "lane retired only {sync_ops} ops — burst did not run the loop");
    assert!(bursts >= 1);
}

#[tokio::test]
async fn kill_switch_means_zero_lane_ops() {
    let src = "let s = 0\nfor (i in 0..1000) { s = s + i }\nprint(s)";
    // vm_run_source_no_sync_lane must keep the counters at 0 (the switch kills).
    // (Extend vm_run_source_lane_stats with a lane-off twin, or have it accept a flag.)
    let (_o, _e, sync_ops, _b) =
        ascript::vm_run_source_lane_stats_no_lane(src).await.expect("ok");
    assert_eq!(sync_ops, 0);
}

#[tokio::test]
async fn lane_on_off_byte_identical_over_core_battery() {
    for src in [
        "print(1 + 2 * 3)",
        "let s = 0\nfor (i in 0..100) { s = s + i }\nprint(s)",
        "let i = 0\nwhile (i < 50) { i = i + 1 }\nprint(i)",
        "print(7 % 3, 2 ** 10, 0xFF & 0b1010, 5 +% 3)",
        "print(`n=${40 + 2}`)",
        "print(1 << 64)",                 // Tier-2 panic path — message identical
    ] {
        let on = ascript::vm_run_source(src).await;
        let off = ascript::vm_run_source_no_sync_lane(src).await;
        match (on, off) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "diverged on `{src}`"),
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "panic diverged on `{src}`"),
            (a, b) => panic!("ok/err disagreement on `{src}`: {a:?} vs {b:?}"),
        }
    }
}
```

- [ ] **Step 2: Run — expect FAIL** (counters stay 0; no driver).
- [ ] **Step 3: Implement the skeleton** (spec §2.2–2.3):

```rust
pub(crate) enum SyncOutcome { Finished(RunOutcome), NeedsAsync }

fn run_loop_sync(&self, fiber: &mut Fiber) -> Result<SyncOutcome, Control> {
    let mut retired: u64 = 0;
    let r = self.sync_burst(fiber, &mut retired);
    if retired > 0 {
        self.lane_sync_ops.set(self.lane_sync_ops.get() + retired);
        self.lane_bursts.set(self.lane_bursts.get() + 1);
    }
    r
}

fn sync_burst(&self, fiber: &mut Fiber, retired: &mut u64) -> Result<SyncOutcome, Control> {
    loop {
        let fault_ip = fiber.frame().ip;
        // SP4 §3 provenance — replicated PER INSTRUCTION, identical to run_loop
        // (run.rs:1092–1096). Hoisting is a recorded follow-up, NOT v1.
        if let Some(src) = fiber.frame().closure.proto.chunk.source.borrow().as_ref() {
            *self.last_fault_source.borrow_mut() = Some(src.clone());
        }
        let byte = fiber.frame().closure.proto.chunk.code[fault_ip];
        let op = Op::from_u8(byte)
            .unwrap_or_else(|| panic!("invalid opcode byte {byte:#x} at ip {fault_ip}"));
        if !sync_lane_op(op) {
            return Ok(SyncOutcome::NeedsAsync); // ip NOT advanced — async re-decodes
        }
        let operand_at = fault_ip + 1;
        fiber.frame_mut().ip = operand_at + op.operand_width();
        match op {
            /* Task 4 subset: Const Nil True False Pop Dup Swap Rot3,
               the binop family via eval_binop_adaptive, Neg/Not/BitNot,
               InstanceOfType, GetLocal SetLocal GetGlobal SetGlobal DefineGlobal
               ImmutableError, Jump Loop JumpIfFalse JumpIfTrue JumpIfNotNil —
               each arm body transcribed from run_loop, delegating to the SAME
               shared helpers; panics via the same panic_at/span construction. */
            _ => unreachable!("sync_lane_op admitted an unimplemented op {op:?}"),
        }
        *retired += 1;
    }
}
```

  `fn sync_lane_op(op: Op) -> bool` is a single `matches!` over the Task-4 subset (it GROWS in
  Tasks 5–6; every op it admits must have an arm — the `unreachable!` is the tripwire, and the
  fuzz axis would catch an escape). Orchestrator: insert at the top of `run_loop`'s `loop`
  (spec §2.3) — `if self.sync_lane { match self.run_loop_sync(fiber)? { Finished(o) => return
  Ok(o), NeedsAsync => {} } }`. **No async arm changes.** NOTE: `Return` is NOT yet in the
  subset, so Task-4 bursts end at frame boundaries — fine, the async arm handles them.
- [ ] **Step 4: Run — expect PASS** on all three new tests; then the FULL differential file +
  `cargo test` both configs + clippy both configs.
- [ ] **Step 5: Commit** — `feat(vm/lane): run_loop_sync core subset + orchestrator burst (LANE §2–3)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer verifies (a) escalation leaves `ip` un-advanced (add a
  unit test driving a fiber whose next op escalates and asserting `frame().ip` unchanged after
  `run_loop_sync`), (b) the transcribed arms match `run_loop`'s byte-for-byte in effect
  (spot-diff), (c) `last_fault_source` handling is identical, (d) counters flush on the `Err`
  path too (a panic mid-burst still records retired ops — check the flush sits OUTSIDE the `?`).

## Task 5: Full sync subset (props/index/builders/match/closures/calls/returns)

**Files:**
- Modify: `src/vm/run.rs` (`sync_lane_op` + `sync_burst` arms)
- Test: `tests/vm_differential.rs`

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn sync_lane_runs_calls_props_and_match_in_lane() {
    // Plain closure calls + property access + match must retire IN the lane:
    // this program contains NO escalation ops after warm-up except print.
    let src = r#"
fn fib(n) { if (n < 2) { return n } return fib(n - 1) + fib(n - 2) }
let o = { x: 0, y: 1 }
for (i in 0..1000) { o.x = o.x + o.y }
let m = match o.x { 1000 => "k", _ => "?" }
print(fib(15), o.x, m)
"#;
    let (out, _e, sync_ops, _b) = ascript::vm_run_source_lane_stats(src).await.expect("ok");
    assert_eq!(out, "610 1000 k\n");
    assert!(sync_ops > 10_000, "calls/props/match must run in-lane, retired {sync_ops}");
}

#[tokio::test]
async fn sync_lane_recursion_depth_panic_is_byte_identical() {
    // SP3 §B: call_depth increments EXACTLY once per call in the lane too.
    let src = "fn f(n) { return f(n + 1) }\nprint(f(0))";
    let on = ascript::vm_run_source(src).await.expect_err("must panic").to_string();
    let off = ascript::vm_run_source_no_sync_lane(src).await.expect_err("must panic").to_string();
    let tw = ascript::run_source(src).await.expect_err("must panic").to_string();
    assert_eq!(on, off);
    assert_eq!(on, tw);
    assert!(on.contains("maximum recursion depth exceeded"));
}

#[tokio::test]
async fn sync_lane_generator_bodies_burst_and_yield() {
    // A generator fiber is driven by the same run() — its body bursts; Op::Yield
    // ends the burst as Finished(Yielded). Behavior identical lane on/off.
    let src = r#"
fn* squares(n) { for (i in 0..n) { yield i * i } }
let g = squares(5)
let total = 0
for await (v in g) { total = total + v }
print(total)
"#;
    let on = ascript::vm_run_source(src).await.expect("ok");
    let off = ascript::vm_run_source_no_sync_lane(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(on.0, "30\n");
}

#[tokio::test]
async fn sync_lane_escalation_battery_is_byte_identical() {
    // Every escalation class, lane-on vs lane-off vs tree-walker:
    for src in [
        "async fn a(x) { return x + 1 }\nprint(await a(41))",       // async call + pending await
        "class C { fn init() { self.n = 7 } fn get() { return self.n } }\nprint(C().get())", // method calls + Class
        "import * as math from \"std/math\"\nprint(math.abs(0 - 5))", // Import + native call
        "enum E { P(a: int) }\nprint(E.P(a: 3).a)",                  // CallNamed
        "let xs = [3, 1, 2]\nprint(xs.map((x) => x * 2))",           // higher-order builtin
    ] {
        let tw = ascript::run_source(src).await.expect("tw ok");
        let on = ascript::vm_run_source(src).await.expect("lane-on ok");
        let off = ascript::vm_run_source_no_sync_lane(src).await.expect("lane-off ok");
        assert_eq!(tw, on.0, "tw vs lane-on diverged on `{src}`");
        assert_eq!(on, off, "lane-on vs lane-off diverged on `{src}`");
    }
}
```

- [ ] **Step 2: Run — expect FAIL** (subset too small; calls escalate).
- [ ] **Step 3: Implement** — grow `sync_lane_op` + arms to the full spec-§3 subset, in this
  order (each sub-group followed by a full `vm_differential` run before the next):
  1. `Return` / `Propagate` / `Unwrap` / `Yield` (shared `return_from_frame`; `Yield` →
     `Finished(RunOutcome::Yielded(v))` after setting `FiberState::Suspended`; **DEFER guard:**
     `Return`/`Propagate` first check `frame.defers.is_empty()` — non-empty returns
     `NeedsAsync` at the un-advanced ip so the async drain arm runs);
  1a. `DeferPush` / `DeferPushMethod` (in-subset per spec §3 "DEFER coordination" — push a
     bound thunk onto `frame.defers`, no call, no await);
  2. `GetIndex SetIndex GetProp GetPropOpt SetProp GetSuper` (shared
     `index_get/index_set/ic_get_field/vm_read_member/vm_set_prop`; the `self.specialize` gate
     transcribed exactly as `run.rs:2601`);
  3. ranges + `CheckNumbers` + `IterSnapshot` + `ArrayLen`;
  4. builders (`NewArray … AppendSpreadArg`) + `Template`;
  5. destructure/match family (`CheckArrayDestructure … MatchNoArm`);
  6. cells/upvalues/param-prologue (`GetLocalCell SetLocalCell FreshCell GetUpvalue SetUpvalue
     CheckParam CheckLocal JumpIfArgSupplied`) + `Closure`;
  7. `Call`/`CallSpread` **conditional**: peek `fiber.stack[callee_idx]` (read-only — for
     `CallSpread` the args array sits on top, compute `callee_idx` accordingly and escalate if
     the peek is not conclusive without popping); a plain `Value::Closure` (`!is_async &&
     !is_worker && !is_generator`) routes through the Task-3 `push_closure_frame`; **every
     other callee kind returns `NeedsAsync` before any pop** (the async arm re-decodes and does
     the popping itself — stack untouched is the invariant).
- [ ] **Step 4: Run — expect PASS** on all four tests; full `cargo test` + clippy, both configs;
  `cargo test --test vm_differential` both configs (the whole-corpus gate now runs with the
  lane ON by default — any corpus divergence is a Task-5 bug to fix here).
- [ ] **Step 5: Commit** (one commit per sub-group is encouraged; final:
  `feat(vm/lane): full sync subset — calls, props, match, builders, returns (LANE §3)`, house trailer).
- [ ] **Reviewer checkpoint:** reviewer audits the `CallSpread` peek math (off-by-one between
  `Call`'s static argc and `CallSpread`'s args-array TOS is the likeliest bug), confirms no pop
  precedes any escalation return, re-runs the corpus differential in BOTH feature configs, and
  fuzzes ad hoc (`cargo fuzz run differential -- -max_total_time=300` if cargo-fuzz is set up
  locally; otherwise `cargo test --test property` — the in-suite generated battery).

## Task 6: Inline ready-future completion (`Op::Await` in the lane)

**Files:**
- Modify: `src/vm/run.rs` (`sync_burst` Await handling; `sync_lane_op` admits `Await`)
- Test: `tests/vm_differential.rs`

- [ ] **Step 1: Write the failing tests:**

```rust
#[tokio::test]
async fn await_on_resolved_future_completes_in_lane() {
    // After the FIRST await parks, the remaining stored futures are resolved →
    // their awaits must complete inline (lane stats prove the burst continued).
    let src = r#"
async fn a(x) { return x * 2 }
let f1 = a(1)
let f2 = a(2)
let f3 = a(3)
let first = await f1            // pending: escalates, scheduler runs the bodies
let rest = (await f2) + (await f3) // resolved by now: inline, in-lane
print(first + rest)
"#;
    let tw = ascript::run_source(src).await.expect("tw ok");
    let on = ascript::vm_run_source(src).await.expect("lane-on ok");
    let off = ascript::vm_run_source_no_sync_lane(src).await.expect("lane-off ok");
    assert_eq!(tw, on.0);
    assert_eq!(on, off);
    assert_eq!(on.0, "12\n");
}

#[tokio::test]
async fn await_identity_on_non_future_stays_in_lane() {
    let (out, _e, sync_ops, _b) =
        ascript::vm_run_source_lane_stats("print(await 5)").await.expect("ok");
    assert_eq!(out, "5\n");
    assert!(sync_ops > 0);
}

#[tokio::test]
async fn inline_take_surfaces_stored_panic_and_propagation_identically() {
    // A resolved-with-Err future must surface the SAME panic via the inline path.
    let panic_src = r#"
async fn boom() { let xs = [1, 2] ; return xs[0].nope() }
let f = boom()
await task_settle()
print(await f)
"#;
    // (Use the corpus idiom for settling: a second resolved await or task.gather —
    // implementer picks the minimal deterministic settle; the assertion is the point.)
    let on = ascript::vm_run_source(panic_src).await;
    let off = ascript::vm_run_source_no_sync_lane(panic_src).await;
    match (on, off) {
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
        (a, b) => panic!("expected identical panics, got {a:?} / {b:?}"),
    }
}

#[tokio::test]
async fn inflight_backpressure_is_byte_identical_lane_on_off() {
    // §3 fairness invariant: >INFLIGHT_YIELD_CAP un-awaited spawns behave identically
    // (every spawn site is an escalation op; the yield runs on the async driver).
    let src = r#"
async fn tick(i) { return i }
let futs = []
for (i in 0..600) { futs = futs.concat([tick(i)]) }
let total = 0
for (f in futs) { total = total + await f }
print(total)
"#;
    let on = ascript::vm_run_source(src).await.expect("ok");
    let off = ascript::vm_run_source_no_sync_lane(src).await.expect("ok");
    let tw = ascript::run_source(src).await.expect("ok");
    assert_eq!(on, off);
    assert_eq!(tw, on.0);
    assert_eq!(on.0, "179700\n");
}
```

  (Implementer: validate each test program against real corpus idioms before relying on it —
  e.g. `futs.concat`, the settle idiom — and adjust syntax to what `examples/**` actually uses.
  The four ASSERTIONS are the contract; the programs may be reshaped.)
- [ ] **Step 2: Run — expect FAIL** (Await escalates always → the lane-stats assertions and the
  in-lane expectations fail; the identity assertions should already pass — if any identity
  assertion fails BEFORE the feature, that is a pre-existing bug: stop and fix it first per the
  production-grade mandate).
- [ ] **Step 3: Implement** (spec §4.1): in `sync_burst`, admit `Op::Await` with peek-first
  handling — peek TOS; non-future → pop/push-back identity (advance ip, retire); `Value::Future`
  with `try_get() == Some(r)` → pop, then `let v = r?` push `v` (advance ip, retire); pending →
  `return Ok(SyncOutcome::NeedsAsync)` with ip un-advanced and the future still on TOS.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs; corpus differential both
  configs (the async examples in `examples/**` are the real exercise).
- [ ] **Step 5: Commit** — `feat(vm/lane): inline ready-future completion at Op::Await (LANE §4)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer specifically probes: a future awaited TWICE (second await
  inline — same value); `race`/`gather` examples from the corpus; a `Propagate`-carrying pair
  through `?` after an inline take; and reads the `try_get` call site to confirm no borrow is
  held while pushing to the fiber.

## Task 7: Differential mode + fuzz axis + corpus coverage assertion (Gate 15)

**Files:**
- Modify: `tests/vm_differential.rs`, `fuzz/fuzz_targets/differential.rs`, `tests/property.rs`
- Test: these ARE the tests

- [ ] **Step 1: Extend the three-way gate to four-way.** In `tests/vm_differential.rs`, every
  place the three-way identity runs (`run_source` == `vm_run_source` == `vm_run_source_generic`
  — the expression batteries, the program batteries, the goldens, the whole-corpus gate), add
  the `vm_run_source_no_sync_lane` projection with the same byte-identical assertion. Follow the
  existing helper-fn structure (extend `assert_vm_matches_treewalker` /
  `assert_vm_run_matches_treewalker` and the corpus runner rather than duplicating loops).
- [ ] **Step 2: Add the corpus coverage assertion** (spec §6.4 — the anti-false-green rule):

```rust
#[tokio::test]
async fn sync_lane_actually_executes_on_the_corpus() {
    // Aggregate sync-lane instruction share over the runnable corpus must be
    // nonzero and is REPORTED so a silent collapse is visible in CI logs.
    let mut total_sync: u64 = 0;
    let mut ran = 0usize;
    for entry in corpus_files() { // the same enumeration the whole-corpus gate uses
        let src = std::fs::read_to_string(&entry).unwrap();
        if let Ok((_out, _exit, sync_ops, _bursts)) =
            ascript::vm_run_source_lane_stats(&src).await
        {
            total_sync += sync_ops;
            ran += 1;
        }
    }
    println!("LANE corpus coverage: {total_sync} sync-lane ops over {ran} programs");
    assert!(ran > 50, "corpus enumeration broke");
    assert!(total_sync > 1_000_000,
        "sync lane retired only {total_sync} ops on the corpus — the lane silently collapsed");
}
```

  (Reuse the whole-corpus gate's file enumeration + skip-list — do not invent a second list.)
- [ ] **Step 3: Fuzz axis, same PR.** `fuzz/fuzz_targets/differential.rs`: add
  `let nolane = project(ascript::vm_run_source_no_sync_lane(&src).await);` to the per-input run
  and include it in the equality assertion + the panic report. Mirror in
  `tests/property.rs::three_way_differential_over_generated_programs` (now four-way) and the
  fixed-seed battery. `cargo build` the fuzz crate (`cd fuzz && cargo build`) to prove it
  compiles even where cargo-fuzz isn't run.
- [ ] **Step 4: Run** — `cargo test --test vm_differential` (BOTH configs; expect the corpus
  gate + coverage assertion green) and `cargo test --test property` (both configs).
- [ ] **Step 5: Commit** — `test(lane): four-way differential mode + fuzz axis + corpus coverage assertion (Gate 15)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs the property suite with a bumped case count, runs
  the fuzzer for ≥10 minutes where available, and **sabotage-tests the coverage assertion**:
  temporarily hard-code `sync_lane_op` to `false` and confirm the coverage test FAILS (then
  revert) — proving the anti-false-green tripwire actually trips.

## Task 8: Bench harness modes + Gate-12/17 re-runs

**Files:**
- Modify: `tests/vm_bench.rs`
- Test: the harness run itself (`--ignored`, release)

- [ ] **Step 1: Add the lane engine + section.** New `Engine::NoSyncLaneVm` wired to
  `ascript::vm_run_source_no_sync_lane`; after `dbg_zero_cost_gate`, add a
  `lane_on_off_overhead` section that times `SpecializedVm` (lane ON, the default) vs
  `NoSyncLaneVm` per benchmark and prints lane-on/lane-off speedups + geomean. GATE: lane-on
  must show **no regression** on any benchmark (`>= 0.97x` noise bound, the existing
  convention); the speedup itself is REPORTED (Task 9's A/B is the headline instrument).
- [ ] **Step 2: Run the full harness** —
  `cargo test --release --test vm_bench -- --ignored --nocapture`. Expected output: the standing
  table + `geomean spec/tw = ≥2.0x` `[PASS]` (Gate 12/17 floor), `dbg_zero_cost_gate` geomean
  `<= 1.05x` `[PASS]` (the lane shares `publish_profile_frames`/`return_from_frame`, so
  armed-idle must stay free), and the new lane section with no `[FAIL]` rows.
- [ ] **Step 3: Record the results** in the `vm_bench.rs` header doc-comment (the GATE RESULT
  convention — append a dated `LANE` block with the actual numbers).
- [ ] **Step 4: Commit** — `bench(lane): lane on/off section + Gate-12/17 re-run results (geomean recorded)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer re-runs the harness independently; if ANY benchmark
  regresses lane-on vs lane-off beyond noise, that is a bug in the burst/orchestrator overhead —
  fixed here, not accepted. Reviewer also re-runs with `ASCRIPT_NO_SPECIALIZE=1`-equivalent
  generic mode mentally checked: the generic×lane combination is covered by the differential;
  spot-time one benchmark on `vm_run_source_generic` to confirm no generic-mode regression.

## Task 9: Same-session A/B + RSS report + post-LANE re-profile (Gates 16, 18; EXEC's gate input)

**Files:**
- Create: `bench/LANE_RESULTS.md`
- Modify: `bench/PROFILING_RESULTS.md`

- [ ] **Step 1: Build the two binaries in one session** — baseline = `main` at the merge-base
  (`git worktree add /tmp/lane-base $(git merge-base HEAD main)` + build), candidate = this
  branch; both `cargo build --profile profiling`.
- [ ] **Step 2: Run the A/B** — `bench/ab.sh /tmp/lane-base/target/profiling/ascript
  target/profiling/ascript 7` over the full 8-workload corpus. Also run the candidate with
  `ASCRIPT_NO_SYNC_LANE=1` through `ab.sh` against itself-lane-on (isolates the lane's own
  contribution from anything else on the branch).
- [ ] **Step 3: Re-profile the async corpus** — `bench/profiling/run.sh` on the candidate;
  capture the bucket attribution for `async_inline`/`async_concurrent` (the **EXEC gate input**:
  is the residual async tax still ≥15%?). Profile at least one workload with the shipped
  profiler (`target/profiling/ascript run --profile cpu bench/profiling/call_heavy.as`) —
  dogfooding is part of Gate 16.
- [ ] **Step 4: Write `bench/LANE_RESULTS.md`** — machine/date/methodology header, the A/B
  table (per-workload medians, speedups, geomean), peak RSS per workload base-vs-candidate
  (Gate 18 — any RSS regression is a bug to fix before merge), the lane-on-vs-lane-off
  isolation table, the async-corpus bucket re-attribution, and an explicit **EXEC gate
  verdict paragraph** (residual async share, with the spec-§8 honesty about
  `async_inline`'s pending-await shape).
- [ ] **Step 5: Append the post-LANE section** to `bench/PROFILING_RESULTS.md` (dated, with the
  re-ranked remaining-spec implications per `goal-perf.md`'s "re-profile checkpoints" rule).
- [ ] **Step 6: Commit** — `bench(lane): same-session A/B + RSS report + post-LANE re-profile (Gates 16/18)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer audits that baseline and candidate ran interleaved in one
  session (the script guarantees it; check the doc says so), numbers in the .md match the raw
  output, RSS did not regress, and the EXEC verdict paragraph follows from the data shown.

## Task 10: Docs, status, holistic review, gates checklist, merge

**Files:**
- Modify: `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md`,
  `superpowers/specs/2026-06-12-two-lane-engine-design.md` (status header)

- [ ] **Step 1: Docs/status updates:**
  - `CLAUDE.md`: a LANE paragraph in the architecture notes (two drivers over one Fiber; the
    sync subset + escalation; `ASCRIPT_NO_SYNC_LANE` beside `--no-specialize`; the
    four-way differential identity; "the orchestrator is the only caller of `run_loop_sync`").
  - `goal-perf.md`: LANE status 🏗️ → ✅ in the spec table; note the Phase-0 corpus shipped;
    record the post-LANE re-profile pointer + the EXEC gate verdict.
  - `superpowers/roadmap.md`: the LANE milestone entry (what shipped, gates, headline numbers).
  - Spec status header: `Draft for review` → `Implemented (merged <sha>)` with deltas-from-spec
    recorded if any (no silent deviation).
  - User-facing `docs/`: no page change required (engine-internal; no surface change) —
    confirm and record that this was checked, per Gate 13.
- [ ] **Step 2: FINAL GATES CHECKLIST** (every box requires pasted command output in the task
  log — evidence before assertions):
  - [ ] `cargo clippy --all-targets` clean AND `cargo clippy --no-default-features
        --all-targets` clean.
  - [ ] `cargo test` green AND `cargo test --no-default-features` green.
  - [ ] `cargo test --test vm_differential` green in BOTH configs (four-way identity + corpus
        + coverage assertion).
  - [ ] `cargo test --test property` green in both configs; fuzz target compiles
        (`cd fuzz && cargo build`); a ≥10-min `cargo fuzz run differential` session where
        available, no findings (or findings fixed in-branch with regression tests).
  - [ ] `cargo test --release --test vm_bench -- --ignored --nocapture`: spec/tw geomean ≥2×,
        `dbg_zero_cost_gate` ≤1.05×, lane section no-regression — all `[PASS]`.
  - [ ] `bench/LANE_RESULTS.md` + post-LANE `PROFILING_RESULTS.md` sections committed; RSS
        reported, no regression.
  - [ ] No `.aso` change: `git diff main -- src/vm/aso.rs src/vm/verify.rs` is empty and
        `ASO_FORMAT_VERSION` unchanged.
  - [ ] Tree-walker untouched: `git diff main -- src/interp.rs` contains no behavioral change
        (test-only/doc-only diffs justified line-by-line).
  - [ ] No new `unwrap/expect/panic!` reachable from user input in the touched code
        (reviewer grep + justification list for VM-bug-invariant panics, which mirror existing
        ones).
- [ ] **Step 3: Holistic review** — a fresh reviewer subagent reviews the WHOLE branch diff
  against the spec: subset table vs `sync_lane_op` (exact match), escalation-stack-untouched
  invariant, SP3 depth accounting, instrument parity, the §4.2 identity argument vs the
  implemented `try_get` path, and hunts latent bugs in neighbors (e.g. `CallSpread` arithmetic,
  `Yield` state handling, counter flush on panic). All findings fixed in-branch with
  regression tests before merge.
- [ ] **Step 4: Merge** — `git checkout main && git merge --no-ff feat/two-lane-engine` with a
  summary merge message (house trailer). Update `goal-perf.md` status table post-merge.

---

## Standing rules for every task (repeated so no subagent misses them)

1. **Bug-fix discipline:** any defect encountered — in LANE code or pre-existing, surfaced
   directly or incidentally — gets a failing-test-first fix **in this branch**, logged in the
   task notes with its commit. A known bug left in the tree is a campaign-blocking defect.
2. **Never relax an assertion** to make a mode agree. The tree-walker is the oracle; lane-off is
   the shipped pre-LANE path; lane-on must equal both.
3. **Both feature configs, every time.** A task is not green until `--no-default-features` is.
4. **No borrow across await** (clippy denies it); the sync driver must contain zero awaits by
   construction — the reviewer greps `\.await` inside `run_loop_sync`/`sync_burst` and expects
   zero hits.
5. **Spans and messages are part of the contract:** every transcribed arm uses the same
   `panic_at`/`span_at`/shared-helper error construction; the differential's panic batteries are
   the proof.
