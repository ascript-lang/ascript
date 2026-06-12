# REGION — Task-Scoped Region Allocation (Probe → Narrow Prototype → GO/NO-GO) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges (code quality + spec adherence) before
> acceptance. At the end of each phase, a **holistic per-phase review subagent** reviews the
> phase's combined changes before the next phase starts. A task/phase is closed only when every
> box under it is ticked.

**Goal:** Execute the REGION spike honestly and record its verdict. Phase 0 audits the
identity-semantics ground truth at HEAD and instruments allocation lifetimes (`region-probe`)
to measure the region-eligible share, with an **early NO-GO checkpoint** (spec §5.3). Phase 1
builds the narrow prototype — proven-dead `ObjectCell` recycling: a heuristic site-selection
pass on `bcanalysis`, a per-Vm `RegionPool`, kill-site wiring guarded by a runtime
`ref_count() == 1` proof — behind `region-spike` + `ASCRIPT_NO_REGIONS`. Phase 2 runs the
same-session A/B (friendly + adversarial) and records **GO or NO-GO against spec §5.5's six
criteria — both outcomes fully specified and first-class** (the honored VAL-rejection
precedent). Phase 3+ (CONDITIONAL on GO) productionizes under the full Gate 1–18 battery.
**Dynamic promote-on-escape is NOT implemented in any phase — spec §2.1 killed it as unsound**
(identity + aliasing are observable semantics); this plan implements only the surviving narrow
design.

**Architecture:** spec `superpowers/specs/2026-06-12-task-regions-design.md` — read it FIRST;
every mechanism, hazard, threshold, and rejection is there. Shape: (1) `region_candidates` in
`src/vm/bcanalysis.rs` (pure analysis — selects kill sites; soundness NEVER rests on it);
(2) a lazily-built per-proto `region_kills` offset bitmap (the `arith_cache` side-table
precedent — `Chunk.code` byte-identical, no `.aso` change); (3) `src/vm/region.rs` —
`RegionPool` (capped `Vec<Cc<ObjectCell>>` + stats) + `RegionScope` task-trim guards at the six
`spawn_local` sites and the `http.serve` request handler; (4) the kill-site check: dying value
is `Object(cc)` ∧ `cc.ref_count() == 1` ∧ pool has room → clear-in-place (retain capacity,
reset `shape`/`frozen`) + pool; `Op::NewObject` pops the pool in region mode. Activation:
specialized VM only ∧ `ASCRIPT_NO_REGIONS` unset; tree-walker + generic VM are permanent
plain-allocator oracles. The uniqueness probe needs a 3-line read-only `ref_count()` getter via
a `[patch.crates-io]` gcmodule fork (spike instrument; production gated per spec §3.2/G6).

**Tech stack:** Rust, single binary `ascript`. Touched: `src/vm/bcanalysis.rs`,
`src/vm/region.rs` (new), `src/vm/run.rs` (NewObject/SetLocal/Pop handlers + `Vm` field),
`src/vm/chunk.rs` (side-table slot), `src/value.rs` (cfg'd probe fields), `src/interp.rs` +
`src/vm/run.rs` spawn sites (probe/scope guards), `src/stdlib/http_server.rs` (request-scope
guard), `Cargo.toml` (`region-probe`/`region-spike` features + the `[patch.crates-io]` fork),
`bench/profiling/region_escape.as` (new), `bench/run_region_bench.sh` (new),
`bench/REGION_RESULTS.md` (the verdict), `tests/vm_differential.rs` + `tests/property.rs` +
`fuzz/` (post-GO), `goal-perf.md` (the row flip).

**Binding execution standards (production-grade mandate, `goal.md` Gates 1–14 +
`goal-perf.md` Gates 15–18):** TDD per task (failing test → minimal code → green → commit,
house trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); any bug
found en route — ours or pre-existing — is fixed in-branch with a failing-test-first regression
guard, never deferred; clippy clean + tests green in BOTH feature configs at every phase close
(plus the `region-probe`/`region-spike` matrices while they exist); never hold a `RefCell`
borrow across `.await`; the tree-walker is never relaxed; no placeholder/TODO on a reachable
path. **Branch:** `feat/task-regions` off `main` (post-NANB/CALL — verify the dependency before
Task 0.0). Merge happens ONLY on a GO verdict with Phase 3 complete; on NO-GO the branch stays
flagged unmerged (the VAL `c1571ec` precedent) except an optional probe-only merge if the
holistic review wants the instrument kept.

**Dependency gate (verify before starting):** NANB merged (the `Value::object/array/...`
constructor seam exists and is the construction chokepoint), CALL merged, the LANE Task-0
server workload present in `bench/`. If any is absent, STOP — this plan's numbers would be
stale by construction (spec front-matter).

---

## File structure

**New files:**
- `src/vm/region.rs` — `RegionPool`, `RegionScope`, stats (Phase 1).
- `src/vm/region_probe.rs` — the `region-probe` instrumentation module (Phase 0).
- `bench/profiling/region_escape.as` — the adversarial high-escape benchmark.
- `bench/run_region_bench.sh` — interleaved same-session A/B runner (mirrors
  `bench/run_compact_value_bench.sh`).
- `bench/REGION_PROBE.md` — Phase-0 histograms + the checkpoint verdict.
- `bench/REGION_RESULTS.md` — the final A/B + GO/NO-GO record.

**Modified:** per Tech stack above. **No grammar, no `.aso`, no docs-site pages** (regions are
invisible infrastructure — the only doc updates are `CLAUDE.md`/`goal-perf.md`/spec, post-GO).

---

## Phase 0 — Ground truth: identity audit + allocation-lifetime probe

### Task 0.1 — Identity-semantics audit (re-verify the spec §2.1 table at HEAD)

The spec's verdict was grounded on 2026-06-12 HEAD; NANB/CALL have merged since. Re-verify
every row so the make-or-break analysis is current, and pin it with tests.

- [ ] Re-run the audit greps and record file:line at current HEAD:
      `grep -n "cc_ptr_eq\|Rc::ptr_eq\|ptr_eq" src/value.rs` (the `PartialEq` identity arms —
      post-NANB these live behind the sealed repr in `value.rs`; confirm the arms are
      unchanged in *semantics*), `MapKey::from_value` (containers must still return `None`),
      every `cc_addr` consumer (`grep -rn "cc_addr" src/`) classified transient-per-call vs
      persistent (the spec table rows 6–7 — there must be NO persistent address-keyed table;
      if one appeared since, that is a NEW spec input: STOP and escalate to the reviewer).
- [ ] Write the **identity battery v0** (`src/vm/region_tests.rs` or `tests/` — runs at HEAD,
      BEFORE any region code, so it pins today's semantics): script-level tests asserting
      (a) `{x:1} == {x:1}` is `false` and `let b = a; a == b` is `true` for all six container
      kinds; (b) alias-mutation visibility (`let b = a; b.x = 2; a.x == 2`); (c)
      `arr.includes(obj)` is identity-based; (d) map/set reject container keys (the existing
      panic) — all four-mode (tree-walker/spec/generic/.aso) via the differential harness.
- [ ] Update the spec §2.1 table in place if ANY row's file:line moved (keep the verdict text;
      this is anchor maintenance, not re-litigation — unless a row's SEMANTICS changed, which
      escalates).
- [ ] Independent review: reviewer re-runs every grep, attempts to find an identity-observable
      operation the table missed (hunt: `instanceof`, `type()`, `assert.same`-style stdlib,
      `lru`/cache modules, REPL `===`-alikes), and confirms the battery actually runs all four
      modes.
- [ ] Commit (house trailer).

### Task 0.2 — The `region-probe` allocation-lifetime instrumentation

Default-off cargo feature `region-probe`; zero overhead when off (fields and Drop impls are
`#[cfg]`-gated OUT — the JIT-counter "not-there" discipline, since this is a dev instrument,
not a shipping seam).

- [ ] `Cargo.toml`: add `region-probe = []` (NOT in `default`).
- [ ] `src/vm/region_probe.rs` (cfg-gated module), the REAL shape:

```rust
//! REGION Phase-0 probe (spec §5.2): per-allocation birth/death accounting keyed
//! by task identity. Dev-only (`--features region-probe`); compiled OUT otherwise.
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SiteClass { Literal, Native }

#[derive(Clone, Copy)]
pub struct Birth { pub task: u64, pub site: SiteClass }

#[derive(Default)]
pub struct ProbeStats {
    // [site_class][died_in_birth_task] -> count, per container kind.
    pub object: [[u64; 2]; 2],
    pub array: [[u64; 2]; 2],
    pub map: [[u64; 2]; 2],
    pub set: [[u64; 2]; 2],
    pub instance: [[u64; 2]; 2],
}

thread_local! {
    static CURRENT_TASK: Cell<u64> = const { Cell::new(0) };   // 0 = main/top-level
    static NEXT_TASK: Cell<u64> = const { Cell::new(1) };
    static LIVE_TASKS: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    static STATS: RefCell<ProbeStats> = RefCell::new(ProbeStats::default());
}

/// RAII task bracket. Installed at every `spawn_local` body and per-request
/// handler; restores the parent task id and retires this id on drop (so a
/// drop AFTER retirement is classified "escaped its task").
pub struct TaskGuard { prev: u64, id: u64 }
pub fn enter_task() -> TaskGuard {
    let id = NEXT_TASK.with(|n| { let v = n.get(); n.set(v + 1); v });
    LIVE_TASKS.with(|l| l.borrow_mut().insert(id));
    let prev = CURRENT_TASK.with(|c| c.replace(id));
    TaskGuard { prev, id }
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        CURRENT_TASK.with(|c| c.set(self.prev));
        LIVE_TASKS.with(|l| l.borrow_mut().remove(&self.id));
    }
}

pub fn birth(site: SiteClass) -> Birth {
    Birth { task: CURRENT_TASK.with(|c| c.get()), site }
}
/// Record a cell's death. `kind` selects the ProbeStats row.
pub fn death(kind: ProbeKind, b: Birth) {
    let in_task = b.task == 0 // main "task" never retires before program end
        || LIVE_TASKS.with(|l| l.borrow().contains(&b.task));
    STATS.with(|s| { /* increment [site][in_task as usize] for kind */ });
}
/// Dump the histogram to `$ASCRIPT_REGION_PROBE_OUT` (JSON lines) at program end.
pub fn dump() { /* env-gated writer; never panics (log + continue on io error) */ }
```

- [ ] Wire births: each of `ObjectCell`/`ArrayCell`/`MapCell`/`SetCell`/`Instance` gains a
      `#[cfg(feature = "region-probe")] probe: Cell<region_probe::Birth>` field, defaulted
      `Native` in the constructors; the VM literal handlers (`Op::NewObject`/`NewArray`,
      `src/vm/run.rs:2296` area) overwrite it to `Literal` right after construction (one cfg'd
      line each). Tree-walker literal sites are NOT classified `Literal` in v1 (region
      activation is VM-only; the probe measures the VM) — note this in the module doc.
- [ ] Wire deaths: cfg'd `impl Drop` for each of the five cells calling
      `region_probe::death(kind, self.probe.get())`. CAUTION: `ObjectCell` etc. must not gain
      a Drop impl in NON-probe builds (a `Drop` impl can change drop-order timing and
      forbid niche optimizations in containers — keep it strictly `#[cfg]`-gated, and run
      the full default-features test suite under `region-probe` to prove behavior is
      unchanged: the probe must never distort what it measures).
- [ ] Wire task brackets: `region_probe::enter_task()` guards (cfg'd) at the six spawn sites
      (`src/interp.rs:5321/:5908/:5982`, `src/vm/run.rs:1747/:5709/:7762` — anchor-verify at
      HEAD) held across each spawned body, and around the `http.serve` per-request handler
      invocation. The guard variable must live across the body's `.await`s (RAII in the async
      block — a plain owned value, no `RefCell` borrow, so the no-borrow-across-await
      invariant is untouched).
- [ ] Dump hook at `run_file`/`vm_run_source` exit + `Vm` teardown (env
      `ASCRIPT_REGION_PROBE_OUT=path`; absent → no output).
- [ ] Tests (under `--features region-probe`): a unit test that a literal object dying in a
      spawned task records `[Literal][in_task]`; one where the object is stored into a global
      and the task ends first records `[Literal][escaped]`; the full default test suite green
      under the feature (no behavioral distortion).
- [ ] Record the **fallback decision point** (spec §5.2): if review finds the field+Drop
      approach distorts timings on the gate workloads (compare wall clock probe-on vs
      probe-off; >3% delta on any gate workload = distorted), implement the
      `#[global_allocator]` interposer fallback instead and note it in `bench/REGION_PROBE.md`.
- [ ] Independent review (runs the suite both configs + the feature matrix; probes the guard
      across cancel-on-drop: spawn, drop the future, assert the task retires).
- [ ] Commit.

### Task 0.3 — Run the probe + the Phase-0 checkpoint (early NO-GO is a real outcome)

- [ ] Build `--release --features region-probe`; run with `ASCRIPT_REGION_PROBE_OUT` over:
      `bench/profiling/json_roundtrip.as`, `bench/profiling/object_churn.as`, the LANE Task-0
      server workload (driven per its harness), and the example corpus (aggregate).
- [ ] Write `bench/REGION_PROBE.md`: machine/date/commit, per-workload histograms
      (kind × site class × in-task/escaped, counts + percentages), and the **checkpoint
      verdict** against spec §5.3: *bytecode-literal allocations dying in-task ≥ 25% of
      allocation events on `json_roundtrip` OR the server workload*.
- [ ] **If the checkpoint FAILS:** this is a full NO-GO. Skip Phases 1–3; execute Task 2.3's
      NO-GO arm (record in `bench/REGION_RESULTS.md` + flip the `goal-perf.md` REGION row to
      evidence-rejected citing the histogram; spec §5.6), then the final holistic review (the
      probe-only merge question). Done.
- [ ] **If it PASSES:** record the eligible shares and proceed to Phase 1.
- [ ] Independent review: reviewer re-runs at least one workload and reproduces the histogram
      within noise; checks the arithmetic of the checkpoint claim.
- [ ] Commit (the report).
- [ ] **Holistic Phase-0 review** (combined Tasks 0.1–0.3) before Phase 1 starts.

## Phase 1 — The narrow prototype (`region-spike`, `NewObject` only)

### Task 1.1 — The gcmodule uniqueness probe (`ref_count()` getter)

- [ ] Fork gcmodule 0.3.3 (git, owner org), one commit exposing:

```rust
impl<T: Trace> Cc<T> {
    /// Read-only strong-count accessor (REGION uniqueness probe).
    /// A fresh `Cc` is 1; clones increment. No other machinery is touched.
    pub fn strong_count(&self) -> usize { self.ref_count() }
}
```

- [ ] `Cargo.toml`: `[patch.crates-io] gcmodule = { git = "...", rev = "..." }` — applied on
      the branch only (the spike instrument; spec §3.2 governs what production requires, G6).
- [ ] Pin tests (in `src/vm/region.rs` tests): fresh tracked `Cc<ObjectCell>` →
      `strong_count() == 1`; a clone → 2; drop the clone → 1; a self-referential object
      (`obj.me = obj` built via the engine) → the cell's count at the would-be kill point ≥ 2.
      (This VERIFIES the tracked-object count semantics rather than assuming them — the
      gcmodule source asserts 1-at-birth, `cc.rs:159-196`; prove clone/drop too.)
- [ ] File the upstream PR (or the owner-noted vendoring decision) and link it in the spec's
      G6 row — the GO gate requires the dependency story resolved, not just patched.
- [ ] Independent review + commit.

### Task 1.2 — `region_candidates` in `bcanalysis` (heuristic site selection)

Pure analysis, no synthesis (the module charter). REMEMBER: soundness does NOT rest on this
pass (spec §3.3) — but precision determines the win, and the forbidden-set walk must still be
honest so the adversarial bench measures misses, not bugs.

- [ ] TDD: unit tests FIRST over hand-compiled chunks (the existing `bcanalysis`/compiler test
      utilities): (a) the loop-churn shape `for ... { let o = {a: i}; use o via GetLocal+GetProp;
      }` → the back-edge `SetLocal` overwrite (or scope-exit `Pop`) of `o`'s slot is selected;
      (b) `arr.push(o)` (Call arg) → NOT selected; (c) `return o` → NOT selected; (d) `g = o`
      (SetGlobal/DefineGlobal) → NOT selected; (e) `o` in `chunk.cell_slots` (captured
      by-reference) → NOT selected; (f) `obj.k = o` (SetProp value position) → NOT selected;
      (g) an `Await`/`Yield` inside the candidate's live range → NOT selected; (h) spread →
      NOT selected.
- [ ] Implement:

```rust
/// REGION (spec §3.1): for one FnProto, the kill-site offsets at which the
/// dying value is a *candidate* for cell recycling. Heuristic by contract —
/// the runtime `strong_count()==1` guard is the soundness proof; this pass
/// only chooses where to pay for that check. Conservative on anything it
/// does not fully model (unknown op in range ⇒ not a candidate).
pub(crate) struct RegionPlan { pub kills: Vec<usize> /* code offsets */ }

pub(crate) fn region_candidates(proto: &FnProto) -> RegionPlan {
    // Linear scan: find `NewObject` immediately followed by `SetLocal s`
    // (the dominant literal-into-local shape). For each, walk forward over
    // the slot's live range tracking ONLY whitelisted uses of `s`:
    //   GetLocal s            — transient read (must itself feed a
    //                           whitelisted consumer: GetProp/SetProp-on-s/
    //                           index-on-s/Pop; anything else disqualifies)
    //   SetProp/SetIndex with s as RECEIVER — fine (mutating the candidate)
    // Disqualifiers (the spec §4 sink census): Return, any Call*, SetGlobal/
    // DefineGlobal, SetUpvalue, slot ∈ chunk.cell_slots, SetProp/SetIndex/
    // Append* with the candidate in VALUE position, spread ops, Await, Yield,
    // jumps leaving the modeled range, or any unmodeled op touching s.
    // The kill site is the next `SetLocal s` overwrite (loop back-edge shape)
    // or the slot's scope-end Pop — record its offset.
}
```

      Reuse the decode/offset walking idioms already in `bcanalysis.rs`
      (`top_level_statement_starts`'s worklist + `verify::op_stack_delta`) — do NOT invent a
      second decoder.
- [ ] Wire the lazy side-table: `FnProto` gains a once-cell `region_kills: OnceCell<Box<[bool]>>`
      (offset-indexed bitmap; built from `region_candidates` on first region-mode execution —
      the `arith_cache` side-table precedent: NOT serialized, no `.aso` touch, no
      `Chunk.code` change). Negative-space test: `ASO_FORMAT_VERSION` unchanged; a golden
      `.aso` byte-compare vs `main`.
- [ ] Independent review (adversarial: reviewer constructs a program where a disqualifier is
      reachable only via a jump and confirms non-selection) + commit.

### Task 1.3 — `RegionPool` + kill-site/alloc-site wiring (the engine change)

- [ ] `Cargo.toml`: `region-spike = []` feature (default-off on the branch during bring-up).
- [ ] `src/vm/region.rs`:

```rust
//! REGION (spec §3): per-Vm pool of proven-dead container cells. A cell enters
//! ONLY via the kill-site check (dying value, strong_count()==1, pool not full)
//! and leaves via Op::NewObject reuse. Pooled cells are emptied (capacity
//! retained), shape/frozen reset, and remain in gcmodule's object space
//! (their Trace visits nothing). NO unsafe.
pub struct RegionPool {
    objects: Vec<Cc<ObjectCell>>,
    cap: usize,                       // ASCRIPT_REGION_POOL_CAP, default 256
    pub stats: RegionStats,           // Cell<u64> counters: recycled, reused,
}                                     // overflow, miss — Gate-18 reporting.

impl RegionPool {
    /// Kill-site path. `dying` was just removed from its slot/stack.
    /// Returns the value back if not recyclable (caller drops it normally).
    pub fn try_recycle(&mut self, dying: Value) -> Option<Value> {
        let Value::Object(cc) = &dying else { return Some(dying) };
        if cc.strong_count() != 1 { self.stats.miss_bump(); return Some(dying); }
        if self.objects.len() >= self.cap { self.stats.overflow_bump(); return Some(dying); }
        let Value::Object(cc) = dying else { unreachable!() };
        {   // reset in place; capacity retained (the win)
            let cell = &*cc;
            cell.map.borrow_mut().clear();
            cell.shape.set(0);
            cell.frozen.set(false);
        }
        self.objects.push(cc);
        self.stats.recycled_bump();
        None
    }
    /// Alloc-site path (Op::NewObject in region mode).
    pub fn take_object(&mut self) -> Option<Cc<ObjectCell>> {
        let cc = self.objects.pop()?; self.stats.reused_bump(); Some(cc)
    }
    /// Task-end trim (RegionScope::drop): release down to a floor.
    pub fn trim(&mut self) { self.objects.truncate(self.cap / 8); }
}
```

      (Adapt field names to the real `ObjectCell` accessors; if `map`/`shape`/`frozen` are
      private, add pub(crate) reset methods ON `ObjectCell` rather than opening the fields.)
- [ ] `Vm` gains `region: Option<RefCell<RegionPool>>` — `Some` iff `specialize` ∧
      `region-spike` built ∧ `ASCRIPT_NO_REGIONS` unset (resolved ONCE at `Vm` construction;
      document beside the `specialize` doc at `run.rs:105`). The off-path in every handler is
      `if let Some(pool) = &self.region` on an `Option` resolved at startup — a single
      predictable branch, and ONLY at flagged offsets (next box).
- [ ] Kill-site wiring: in the `SetLocal` (overwrite) and `Pop` handlers, the region check
      runs ONLY when `self.region.is_some()` AND the current proto's `region_kills` bitmap
      (built lazily here via `region_candidates`) flags the current offset. No `await` in
      reach; the pool borrow is scoped to the handler body (clippy
      `await_holding_refcell_ref` stays clean by construction).
- [ ] Alloc-site wiring: `Op::NewObject` (`run.rs:2296`) pops `take_object()` when region mode
      is on and a cell is available; the reused cell then takes the exact same shape-assignment
      path a fresh cell takes (the SHAPE registry codepath must be IDENTICAL — reviewer
      verifies no shape-id staleness).
- [ ] `RegionScope` guards (trim-on-drop) at the six spawn sites + the `http.serve` request
      handler (reuse the Task-0.2 bracket seam; in `region-spike` builds the guard trims, in
      `region-probe` builds it records, in plain builds it does not exist).
- [ ] Coverage assertion test: run a literal-churn corpus program in region mode; assert
      `stats.recycled > 0` AND `stats.reused > 0` (the anti-false-green rule, Gate 15).
- [ ] Spike differential: extend `tests/vm_differential.rs` with a region axis (feature-gated):
      tree-walker == specialized+regions == specialized+`ASCRIPT_NO_REGIONS` == generic over
      corpus + goldens, both feature configs. Identity battery (Task 0.1) re-run in region
      mode PLUS the new §6.4 adversarial cases: alias-mutate-observe with a recycle between
      loop iterations; `==` across iterations at a reused address; `obj.me = obj` self-edge
      (assert `miss` bumped, value correct, and `gc::tracked_count` returns to baseline after
      `collect()` — the cycle path still works); freeze/airlock round-trips over reused cells;
      frozen-flag/shape never leak into a successor (construct, freeze, recycle-attempt → must
      MISS since freeze implies an extra ref? — if it CAN hit with count 1, the reset clears
      `frozen`; test BOTH branches).
- [ ] Full suite + clippy, both feature configs, plus the spike matrix.
- [ ] Independent review (must include: an attempt to construct a wrong-reuse divergence by
      hand; verification that regions-off builds contain zero region branches on unflagged
      offsets; the differential + coverage runs) + commit.
- [ ] **Holistic Phase-1 review** before Phase 2.

## Phase 2 — A/B + the verdict

### Task 2.1 — The adversarial benchmark + the bench harness

- [ ] `bench/profiling/region_escape.as`: (a) a loop constructing object literals and
      appending EVERY one to a retained array (kill checks all miss via the Call/Append
      disqualify — so to actually exercise the RUNTIME miss path, include a variant shaped to
      pass the static pass but fail the refcount: e.g. alias the object into a second local
      before the overwrite); (b) a callback-passing variant. Assert via stats that the run
      records misses and ZERO recycles (the benchmark must measure what it claims).
- [ ] `bench/run_region_bench.sh`: same-session interleaved A/B (the
      `run_compact_value_bench.sh` protocol): baseline = branch build with
      `ASCRIPT_NO_REGIONS=1`, candidate = regions on; workloads = `json_roundtrip`, the server
      workload, `object_churn`, `region_escape`, + the full bench corpus sweep; instruments =
      wall clock (median ≥5), `ascript run --profile cpu` (allocation-attribution delta),
      `/usr/bin/time -l` (RSS), pool stats, allocation counts (Gate 18).
- [ ] Zero-cost-off leg: branch-with-`ASCRIPT_NO_REGIONS` vs `main` (pre-REGION) — must be
      ≈1.00× geomean; re-run `dbg_zero_cost_gate`.
- [ ] Independent review + commit.

### Task 2.2 — Run the A/B

- [ ] Execute `bench/run_region_bench.sh`; capture raw outputs under `bench/out/`.
- [ ] Evaluate spec §5.5 G1–G6 one by one with measured numbers (G6 = the upstream-PR /
      vendoring status from Task 1.1).
- [ ] Independent review: reviewer re-runs at least the gate workloads and confirms the
      numbers reproduce within noise; audits the attribution methodology (G1 must be a real
      end-to-end win, not a profiler re-bucketing).

### Task 2.3 — Record the verdict (BOTH outcomes fully specified)

- [ ] Write `bench/REGION_RESULTS.md` in the `COMPACT_VALUE_RESULTS.md` format:
      machine/date/commits, the Phase-0 histograms (linked), the full A/B table (all modes),
      pool-stat tables, RSS/alloc tables, each §5.5 criterion with its measured value, and the
      explicit verdict.
- [ ] **NO-GO arm:** flip the `goal-perf.md` REGION row to evidence-rejected with the headline
      numbers (the honored VAL precedent); annotate the spec front-matter Status →
      "Evidence-rejected <date>, see bench/REGION_RESULTS.md"; leave `feat/task-regions`
      flagged & unmerged; decide (holistic review) whether the Phase-0 probe merges alone
      (default-off feature; merge only if judged a durable instrument — otherwise it stays on
      the branch). Run the final holistic review. **Done — this is a first-class campaign
      outcome.**
- [ ] **GO arm:** update `goal-perf.md` REGION row → 🟡 in progress (gate met, numbers cited);
      proceed to Phase 3.
- [ ] Commit.
- [ ] **Holistic Phase-2 review.**

## Phase 3 — Productionization (CONDITIONAL — execute only on a recorded GO)

### Task 3.1 — Promote the spike to default-on

- [ ] Resolve G6 concretely: gcmodule getter upstreamed-and-released (point `Cargo.toml` at
      the release) OR the owner-noted vendored fork recorded in `CLAUDE.md` + `goal-perf.md`.
      The `[patch.crates-io]` entry must NOT survive to `main` without that record.
- [ ] Remove the `region-spike` feature: region code compiles unconditionally;
      `ASCRIPT_NO_REGIONS` remains the permanent kill switch (Gate 15 — mirrors
      `--no-specialize`, documented beside it); generic mode + tree-walker remain structurally
      plain.
- [ ] Region differential modes become permanent (non-feature-gated) in
      `tests/vm_differential.rs`, BOTH feature configs, coverage assertion FAILING (not
      warning) on zero recycles.
- [ ] Full suite + clippy both configs; the Gate-17 spec/tw ≥2× floor re-verified.
- [ ] Independent review + commit.

### Task 3.2 — Fuzz axis + soak + leak battery

- [ ] Differential fuzzer: add the regions axis with the allocation-heavy grammar weight
      (literal density, churn loops, aliasing/self-reference/capture patterns — spec §6.3);
      the fuzz harness reports the recycle counter (anti-false-green). Time-boxed
      `FUZZ_STRESS_N` campaign on the branch (the FUZZ ~284k precedent); any divergence is a
      bug fixed with a failing-test-first guard.
- [ ] Leak/lifecycle tests (spec §6.5): pool-cap overflow fallback (stat-asserted);
      cancel-storm trim (spawn N tasks, drop futures, assert pool + `tracked_count` bounded);
      panic-unwind pool release; server soak with regions on (the V13-T5 pattern — live
      tracked set bounded near the growth threshold); REPL cross-line persistence unaffected.
- [ ] Independent review + commit.

### Task 3.3 — Staged site-class extension (`NewArray`, then evaluate the rest)

- [ ] Extend `region_candidates` + pool to `ArrayCell` (`Op::NewArray`), TDD as Task 1.2,
      per-class A/B (same harness): keep only if the class is a measured win (each class
      clears its own mini-gate; a losing class is recorded and excluded — no
      assume-it-generalizes).
- [ ] Evaluate `Map`/`Set`/`Instance` the same way; record keep/exclude per class in
      `bench/REGION_RESULTS.md`.
- [ ] Independent review + commit.

### Task 3.4 — Docs, bookkeeping, final gates

- [ ] `CLAUDE.md`: a REGION paragraph in the campaign/subsystem notes (the recycling design,
      the kill switch, the identity-soundness split, the gcmodule getter dependency, the
      VM-only activation + oracle posture). `goal-perf.md` REGION row → ✅ with the headline
      number. Spec status → implemented-as-amended (any deltas recorded, no silent drift).
      Roadmap entry per house convention. NO docs-site pages (invisible infrastructure —
      record that explicitly so it is not mistaken for staleness).
- [ ] Final full-gates checklist (run, evidence captured, no assertion relaxed):
      - [ ] Gate 1 four-mode byte-identity incl. the region modes, both feature configs
      - [ ] Gate 2 clippy clean `--all-targets` AND `--no-default-features --all-targets`
      - [ ] Gate 3 `cargo test` + `cargo test --no-default-features` green
      - [ ] Gate 4 no borrow across `.await`; native handles stay GC-opaque (region pool never
            touches a native kind)
      - [ ] Gate 5 zero `type-*` false positives on `examples/**` (static checker untouched —
            verify anyway)
      - [ ] Gates 6/14 no placeholders/silent deferrals; every excluded site class is recorded
      - [ ] Gates 9/10 examples + unit tests happy AND edge (the identity battery, the leak
            battery, overflow/cancel/panic edges)
      - [ ] Gate 11 tooling parity confirmed-working (no surface change → conformance suites
            green as the proof)
      - [ ] Gate 12/17 zero-cost-off proven (A/B + `dbg_zero_cost_gate`); spec/tw geomean ≥2×
      - [ ] Gate 15 region modes in differential + fuzzer with coverage assertions; kill
            switch permanent
      - [ ] Gate 16 same-session A/B recorded in `bench/REGION_RESULTS.md`
      - [ ] Gate 18 RSS + allocation counts reported; no memory regression
- [ ] **Final holistic review** (whole-branch) → merge `feat/task-regions` into `main`
      `--no-ff`.
