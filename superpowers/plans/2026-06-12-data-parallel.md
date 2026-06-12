# Data-Parallel Primitives over the Frozen Shared Heap (PAR) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship `task.pmap(data, f, opts?) -> future<array>` and
`task.preduce(data, f, init, opts?) -> future<T>` as ordinary `std/task` functions that chunk
an array across the existing worker pool (frozen `Value::Shared` input crosses by `Arc` bump
via the shipped `TAG_SHARED` side-vector; plain-array input crosses by per-chunk
structured-clone — today's worker-arg semantics), run the named-`worker fn` callback inside
isolates via a native per-chunk driver, and merge results **in input order** with
first-by-input-order error selection — byte-identical across tree-walker / specialized-VM /
generic-VM / `.aso`, with no syntax, no opcode, no `.aso` bump, and no new serializer tag.

**Spec:** `superpowers/specs/2026-06-12-data-parallel-design.md` (PAR). **Read it first and in
full** — §2 (API + callback contract), §3.1 (the frozen-vs-copy input decision and WHY
auto-freeze was rejected), §3.3 (chunk plan + driver), §3.5 (error/cancel semantics), §4 (the
failure-mode table — every row becomes a test). Section references (§) below are into it.

**Before writing any code, read these files end to end** (they are the machinery being
composed; the spec's line numbers were verified 2026-06-12 — **re-grep every symbol before
editing**, names are the anchors):
- `src/stdlib/task_mod.rs` (the API home; `call_task`, `gather`, `SharedFuture` patterns)
- `src/worker/mod.rs` (`dispatch_worker`, `run_slice_inline`, the bridge + `InflightGuard`)
- `src/worker/isolate.rs` (`WorkerRequest`, `isolate_loop`, the biased abort select)
- `src/worker/dispatch.rs` (slice builders; the in-process test helpers at the bottom)
- `src/worker/pool.rs` (`send_to` ship-once mirror — do NOT break its field-agnostic flow)
- `src/stdlib/shared.rs` + the `Shared` readers in `src/interp.rs` (`shared_to_value_shallow`
  and the shared array len/child helpers near `interp.rs:7298+`)
- `src/interp.rs` `call_run_in_worker` (~`:6025`) + `worker_fn_dispatch_name` (~`:7753`)

**Architecture:** Phase 1 (Unit A — transport + driver): `ChunkJob` on `WorkerRequest`, the
native `run_chunk_job` element loop in `src/worker/isolate.rs`, threading through
`dispatch_worker`/`run_slice_inline` (`src/worker/mod.rs`). Phase 2 (Unit B — stdlib
surface): validation, the chunk planner, the orchestrator + inline same-decomposition
executor, the final-combine stage (`src/stdlib/task_mod.rs`). Phase 3 (Unit C — corpus +
negative space + docs): examples, differential, `tests/par_negative_space.rs`, Gate-5 sweep,
docs. Phase 4 (Unit D — bench + finish): `bench/data_parallel_bench.as` + runner + report,
RSS, vm_bench re-run, CLAUDE/roadmap/goal-perf updates, holistic review.

**Tech stack:** Rust; the `!Send` per-isolate runtime (never add `Send` bounds; never hold a
`RefCell` borrow across `.await`); tokio `current_thread` + `LocalSet`; tests via
`cargo test` in BOTH feature configs; `tests/vm_differential.rs`; `/usr/bin/time -l` for RSS.

**Hard rules carried from the spec:**
- **No new wire tag, no `Op`, no `ASO_FORMAT_VERSION` change** (it is 27 at writing,
  `src/vm/aso.rs:167` — the negative-space test pins it). `ChunkJob` is plain `Send` fields
  on `WorkerRequest`, like `caps`.
- **Unfrozen input = per-chunk airlock copy** (spec §3.1) — do NOT auto-freeze. Frozen input
  = the `TAG_SHARED` `Arc`-bump path. Non-array → the §4 panic.
- **Input-order everything:** result merge in chunk order; errors selected by awaiting chunk
  futures in input order; nested/degraded execution runs the SAME chunk decomposition
  (§5.1 venue-invariance — required for non-associative `preduce` byte-identity).
- **Callback = named top-level `worker fn` only** (reuse `worker_fn_dispatch_name`; mirror
  `run_in_worker`'s panic wording).
- Per-element control flow mirrors `isolate_loop` exactly: `Propagate` → `nil` element,
  `Exit` → the shipped refusal, `Panic` → chunk panic (spec §3.3.2).
- `std/task` is CORE (no feature gate) — everything must build and pass under
  `--no-default-features`.

**Binding execution standards (production-grade mandate):** any bug found while working —
ours or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first
regression guard, never stepped around (goal.md Gate 14). No placeholders, no silent
deferrals. Branch: `feat/data-parallel` off `main`. Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `tests/par_negative_space.rs` — ASO version pin, serializer-tag pin, no-new-`Op` pin.
- `examples/data_parallel.as` (intro) + `examples/advanced/data_parallel_pipeline.as`
  (production-shaped) — Gate 9 happy+edge, four-mode corpus members.
- `bench/data_parallel_bench.as`, `bench/run_data_parallel_bench.sh`,
  `bench/DATA_PARALLEL_RESULTS.md`.

**Modified files:**
- `src/worker/isolate.rs` — `ChunkJob`/`ChunkKind`; `WorkerRequest.chunk`; `run_chunk_job`;
  the one-arm `isolate_loop` change.
- `src/worker/mod.rs` — `dispatch_worker_job(.., Option<ChunkJob>)` (public `dispatch_worker`
  delegates with `None`); `run_slice_inline` chunk handling; `dispatch_worker_dedicated`
  passes `None`.
- `src/stdlib/task_mod.rs` — `pmap`/`preduce` exports + `call_task` arms + planner +
  orchestrator + inline executor + tests.
- `src/check/std_arity.rs` — `task.pmap`/`task.preduce` entries **iff** the table curates
  `std/task` (Task 2.1 verifies; if `std/task` is absent, add nothing — note it in the
  commit message).
- `docs/content/stdlib/async.md` (the `std/task` reference — there is NO `task.md`; this is
  the verified home) + `docs/content/language/workers.md` + `README.md` stdlib line.
- `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md` — final task.

---

## Phase 0 — Preflight: branch, semantic pins

### Task 0.1: branch + pin the inherited semantics PAR composes

**Files:** create the test scaffold in `src/stdlib/task_mod.rs` `#[cfg(test)]` (run_source
style, like the existing `retry_*` tests).

- [ ] **Step 1:** `git checkout -b feat/data-parallel main`. `cargo build --release` clean.
- [ ] **Step 2:** Write PASSING pin tests for the shipped behaviors the design leans on
  (these document today's semantics; if any fails, STOP — the spec's ground truth moved):

```rust
// PAR Phase 0 pins — shipped semantics the pmap/preduce design composes (spec §3).
#[tokio::test]
async fn pin_worker_propagate_yields_nil() {
    // Top-level `?` inside a worker body resolves the call to nil (isolate_loop rule).
    let out = run(r#"
worker fn t(x) {
    let [v, e] = [nil, {message: "nope"}]
    let r = [v, e]?
    return 1
}
print(await t(0))
"#).await;
    assert_eq!(out, "nil\n");
}

#[tokio::test]
async fn pin_frozen_array_arg_crosses_and_reads_in_worker() {
    // A frozen array arg is readable per element inside a worker (TAG_SHARED path).
    let out = run(r#"
import * as shared from "std/shared"
worker fn pick(arr, i) { return arr[i] * 10 }
let f = shared.freeze([1, 2, 3])
print(await pick(f, 1))
"#).await;
    assert_eq!(out, "20\n");
}

#[tokio::test]
async fn pin_worker_fn_dispatch_name_rules() {
    // run_in_worker's callback rule (named worker fn only) — pmap mirrors it (spec §2.2).
    let out = run(r#"
import * as caps from "std/caps"
let [v, err] = recover(() => run_in_worker((x) => x, 1))
print(err != nil)
"#).await;
    assert_eq!(out, "true\n");
}
```

  (Adjust the third pin to however `run_in_worker` is actually imported/exposed — grep
  `"run_in_worker"` in `src/stdlib/` and `global_env`; the assertion is the panic on a
  non-`worker fn` callee.)
- [ ] **Step 3:** `cargo test task_mod` green in BOTH configs. Commit —
  `test(par): phase-0 pins for the worker semantics pmap composes (spec §3)`.

### Task 0.2: Phase 0 review

- [ ] Independent reviewer: pins assert live behavior (run them against the release binary
  by hand-translating to `.as` files); confirm `ASO_FORMAT_VERSION` current value recorded
  in the branch notes (read `src/vm/aso.rs`); confirm no source files changed besides tests.

---

## Phase 1 — Unit A: the chunk transport + native driver

### Task 1.1: `ChunkJob` + `WorkerRequest.chunk` (failing test first)

**Files:** modify `src/worker/isolate.rs`, `src/worker/mod.rs`, `src/worker/pool.rs` (test
fixture only — `req_with_bytes` gains `chunk: None`).

- [ ] **Step 1 (failing test):** in `src/worker/isolate.rs` tests, an in-process driver test
  (the `dispatch.rs` `run_slice_in_fresh_isolate` style — no pool, no threads):

```rust
/// PAR Unit A: the native chunk driver maps a slice of a plain data array through the
/// entry, collecting results in order; Propagate elements become nil; a panic aborts.
#[tokio::test]
async fn chunk_driver_map_plain_array_in_order() {
    let src = "worker fn double(x) { return x * 2 }";
    let top = crate::compile::compile_source(src).expect("compiles");
    let slice = crate::worker::build_code_slice(&top, "double", None).expect("slice");
    // Fresh isolate-equivalent Vm, load the slice, then drive a Map chunk over [10,20,30]
    // with start=0..3 and assert [20,40,60] in order.
    // (Reuse/lift dispatch.rs's run_slice_in_fresh_isolate scaffolding into a shared
    //  test helper rather than copying it.)
    ...
    let data = Value::Array(crate::value::ArrayCell::new(vec![
        Value::Int(10), Value::Int(20), Value::Int(30),
    ]));
    let job = ChunkJob { kind: ChunkKind::Map, start: 0, end: 3 };
    let out = run_chunk_job(&vm, entry, data, &job, Span::new(0, 0)).await.unwrap();
    assert_eq!(format!("{out}"), "[20, 40, 60]");
}
```

- [ ] **Step 2:** add the types + field (spec §3.3.2):

```rust
/// PAR (spec §3.3.2): a per-chunk data-parallel job. Plain Copy/Send scalars riding
/// beside `caps` — NOT part of the structured-clone byte stream (no new wire tag;
/// pinned by tests/par_negative_space.rs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChunkKind { Map, Reduce }

#[derive(Clone, Copy, Debug)]
pub struct ChunkJob { pub kind: ChunkKind, pub start: u32, pub end: u32 }
```

  `WorkerRequest` gains `pub chunk: Option<ChunkJob>`; every existing construction site
  (`dispatch_worker` in `mod.rs`, the pool test fixture) sets `None`.
- [ ] **Step 3:** implement `run_chunk_job` in `isolate.rs`:

```rust
/// PAR (spec §3.3.2): the native per-chunk element loop. `data` is either the WHOLE
/// frozen input array (`Value::Shared`, frozen path — `start..end` index into it) or
/// this chunk's own plain element slice (copy path — start=0). Per-element control
/// flow mirrors the worker top-level rules in `isolate_loop` EXACTLY: Ok → result
/// (a returned future is driven first), Propagate → nil, Exit → the shipped refusal,
/// Panic → the chunk fails. Map replies an ordered results array; Reduce replies the
/// partial fold seeded with the chunk's FIRST element (init never enters a chunk —
/// spec §3.3.3).
pub(crate) async fn run_chunk_job(
    vm: &Rc<Vm>,
    entry: Value,
    data: Value,
    job: &ChunkJob,
    span: crate::span::Span,
) -> Result<Value, crate::interp::Control> {
    use crate::interp::Control;
    let len = chunk_data_len(&data, span)?;
    let (start, end) = (job.start as usize, (job.end as usize).min(len));
    let element = |i: usize| chunk_data_element(&data, i, span);

    // One element call with the worker top-level control-flow rules applied.
    async fn call_element(
        vm: &Rc<Vm>, entry: &Value, args: Vec<Value>, span: crate::span::Span,
    ) -> Result<Value, Control> {
        match vm.call_value(entry.clone(), args, span).await {
            Ok(Value::Future(f)) => f.get().await,           // drive an async body's handle
            Ok(v) => Ok(v),
            Err(Control::Propagate(_)) => Ok(Value::Nil),    // top-level `?` rule
            Err(Control::Exit(_)) => Err(Control::Panic(crate::error::AsError::at(
                "exit() is not allowed inside a worker".to_string(), span,
            ))),
            Err(other) => Err(other),
        }
    }

    match job.kind {
        ChunkKind::Map => {
            let mut out = Vec::with_capacity(end.saturating_sub(start));
            for i in start..end {
                out.push(call_element(vm, &entry, vec![element(i)?], span).await?);
            }
            Ok(Value::Array(crate::value::ArrayCell::new(out)))
        }
        ChunkKind::Reduce => {
            let mut acc = element(start)?;                   // seed: first element
            for i in start + 1..end {
                acc = call_element(vm, &entry, vec![acc, element(i)?], span).await?;
            }
            Ok(acc)
        }
    }
}
```

  `chunk_data_len`/`chunk_data_element`: match `Value::Array` (borrow len / indexed clone —
  never hold the borrow across the `.await`: clone the element OUT before calling) and
  `Value::Shared` whose node is `SharedNode::Array` (reuse the shipped SRV readers — grep
  `shared_to_value_shallow` / the shared array child helper near `src/interp.rs:7298+` and
  call the SAME function the `Shared` index read uses, so element materialization is
  byte-identical to `frozen[i]` in script). Any other `data` kind → an internal-invariant
  recoverable panic (the stdlib layer validated already).
- [ ] **Step 4:** wire `isolate_loop`: after the existing entry fetch, replace the single
  `vm.call_value(entry, arg_values, span)` run future with:

```rust
let data0 = arg_values.into_iter().next().unwrap_or(Value::Nil);
let run = async {
    match &chunk {
        Some(job) => run_chunk_job(&vm, entry, data0, job, crate::span::Span::new(0, 0)).await,
        None => vm.call_value(entry, /* original args */, crate::span::Span::new(0, 0)).await,
    }
};
```

  — keeping the surrounding `tokio::pin!` + biased-abort `select!` + reply-encode envelope
  **byte-for-byte** (the `None` arm must remain today's exact path; structure the change so
  the non-chunk path does not move).
- [ ] **Step 5:** the driver tests green (Map order, Reduce seed-with-first, Propagate→nil
  element, Panic aborts mid-chunk, frozen `Shared` data path, `end > len` clamps). Edge
  tests: empty range (`start == end`) → Map `[]` / Reduce is unreachable from the planner
  (assert the planner never emits it in Phase 2; here return a recoverable internal panic,
  never an unwrap). `cargo test --lib worker` + clippy both configs.
- [ ] **Step 6:** Commit — `feat(worker/par): ChunkJob + native per-chunk driver in the
  isolate loop (PAR spec §3.3.2)`.

### Task 1.2: thread the job through dispatch + the inline fallback

**Files:** modify `src/worker/mod.rs`.

- [ ] **Step 1 (failing test):** an end-to-end pooled chunk dispatch test (testrun-style —
  grep `src/worker/testrun.rs` for the harness pattern): build a slice for
  `worker fn double`, `dispatch_worker_job` with `ChunkJob{Map,0,3}` and plain data
  `[1,2,3]`, await the future on a `LocalSet`, assert `[2,4,6]`.
- [ ] **Step 2:** rename the body of `dispatch_worker` to
  `pub(crate) fn dispatch_worker_job(interp, slice, data_args: Vec<Value>, chunk:
  Option<ChunkJob>, span)`; the public `dispatch_worker` delegates with `None`
  (signature unchanged — zero churn at existing call sites). The job sets
  `req.chunk = chunk`; everything else (sendability gate, encode-as-one-array, caps floor,
  bridge, `InflightGuard`) is untouched.
- [ ] **Step 3:** `run_slice_inline` gains the `chunk: Option<ChunkJob>` parameter (the
  graceful-degradation `Err(req)` path forwards `req.chunk`) and, inside its spawned task,
  routes through the SAME `run_chunk_job` when `Some` — venue-invariance (spec §5.1).
  `dispatch_worker_dedicated` constructs no chunk (PAR is pooled-only) — it doesn't build a
  `WorkerRequest`, so only confirm it compiles untouched.
- [ ] **Step 4:** tests green; clippy clean BOTH configs;
  `cargo test --no-default-features --lib worker` green (core path, no features).
- [ ] **Step 5:** Commit — `feat(worker/par): thread ChunkJob through pooled dispatch + the
  inline degradation path (PAR spec §3.3/§3.5)`.

### Task 1.3: Phase 1 independent review

- [ ] Reviewer runs the driver + dispatch tests, then probes: (a) cancellation — dispatch a
  slow chunk, drop the future, assert the isolate replies `Cancelled`/discard without
  wedging the pool (`InflightGuard` decrements — re-dispatch succeeds after); (b) the
  `None`-chunk path is byte-identical (run the full existing worker test battery:
  `cargo test worker` both configs, `cargo test --test vm_differential` both configs);
  (c) no borrow held across an await in the new driver (clippy `await_holding_refcell_ref`
  is deny — confirm it actually ran on the new code); (d) `WorkerRequest` stays fully
  `Send` (compile is the proof; reviewer confirms no `Rc`/`Value` snuck into `ChunkJob`).

---

## Phase 2 — Unit B: `task.pmap` / `task.preduce`

### Task 2.1: validation + the chunk planner (pure, unit-tested)

**Files:** modify `src/stdlib/task_mod.rs`; maybe `src/check/std_arity.rs`.

- [ ] **Step 1 (failing tests):** pure planner tests:

```rust
#[test]
fn chunk_plan_contract() {
    // spec §3.3.1 — the published formula. (len, cap, min_chunk) -> boundaries.
    assert_eq!(chunk_plan(10, 4, 1), vec![(0, 3), (3, 6), (6, 9), (9, 10)]);
    assert_eq!(chunk_plan(3, 8, 1), vec![(0, 1), (1, 2), (2, 3)]);   // chunks > len clamps
    assert_eq!(chunk_plan(100, 8, 16), vec![(0, 16), (16, 32), (32, 48), (48, 64),
                                            (64, 80), (80, 96), (96, 100)]);
    assert_eq!(chunk_plan(5, 1, 1), vec![(0, 5)]);
    assert!(chunk_plan(0, 8, 1).is_empty());
}
```

  (Lock the formula: `chunk_size = max(min_chunk, len.div_ceil(cap))`,
  `boundaries = (0..len).step_by(chunk_size)` pairs — write it exactly once.)
- [ ] **Step 2:** implement `chunk_plan(len, cap, min_chunk) -> Vec<(usize, usize)>` +
  `pool_cap()` (`$ASCRIPT_WORKERS` positive-int else `num_cpus::get().max(1)` — mirror
  `pool.rs:59-64`; do NOT couple to the pool's private state) + opts parsing
  (`{chunks?, minChunk?}`, positive-int validation mirroring `task.retry`'s opts errors,
  unknown keys ignored like other stdlib opts) + input classification:

```rust
/// PAR (spec §3.1): classify pmap/preduce input. Frozen array → zero-copy path
/// (the whole Shared ships per chunk + index range); plain array → snapshot now,
/// per-chunk copy path; anything else → the §4 panic.
enum ParInput {
    Frozen { shared: Value, len: usize },        // Value::Shared(SharedNode::Array)
    Plain { elems: Vec<Value> },                 // snapshot taken AT CALL TIME
}
```

  Non-array (incl. `Shared` of a non-array → `got frozen <kind>`) panics with
  `task.pmap expects an array or a frozen array (got <kind>)`.
- [ ] **Step 3:** callback validation: `worker_fn_dispatch_name(&f)` (it is module-private in
  `interp.rs` — promote to `pub(crate)` or re-export; do NOT duplicate it) else panic
  `` task.pmap expects a named `worker fn` as its callback (got <kind>) ``.
- [ ] **Step 4:** check `src/check/std_arity.rs` for existing `std/task` curation (grep
  `"task"`). If curated, add `pmap` (min 2) / `preduce` (min 3) with `max=None`; if
  `std/task` is not in the table, add nothing and say so in the commit body.
- [ ] **Step 5:** tests green both configs. Commit —
  `feat(task/par): input classification + the contractual chunk planner (PAR spec §3.1/§3.3.1)`.

### Task 2.2: the orchestrator — pmap end to end

**Files:** modify `src/stdlib/task_mod.rs`.

- [ ] **Step 1 (failing tests, run_source):**

```rust
#[tokio::test]
async fn pmap_plain_array_input_order() {
    let out = run(r#"
import * as task from "std/task"
worker fn double(x) { return x * 2 }
print(await task.pmap([1, 2, 3, 4, 5, 6, 7, 8], double))
"#).await;
    assert_eq!(out, "[2, 4, 6, 8, 10, 12, 14, 16]\n");
}

#[tokio::test]
async fn pmap_frozen_input_same_result() {
    let out = run(r#"
import * as task from "std/task"
import * as shared from "std/shared"
worker fn double(x) { return x * 2 }
print(await task.pmap(shared.freeze([1, 2, 3]), double, { chunks: 2 }))
"#).await;
    assert_eq!(out, "[2, 4, 6]\n");
}

#[tokio::test]
async fn pmap_empty_is_instant_and_poolless() {
    let out = run(r#"
import * as task from "std/task"
worker fn id(x) { return x }
print(await task.pmap([], id))
"#).await;
    assert_eq!(out, "[]\n");
    assert!(!crate::worker::pool_is_initialized(), "empty pmap must not touch the pool");
}
```

- [ ] **Step 2:** implement (`call_task` gains `"pmap"`/`"preduce"` arms; exports gain
  `("pmap", bi("task.pmap"))`, `("preduce", bi("task.preduce"))`):

```rust
/// `pmap(data, f, opts?) -> future<array>` (PAR spec §2/§3.4). Synchronous inside the
/// call: validate, SNAPSHOT the input, plan chunks, build the slice once, dispatch
/// every chunk (eager). Then an orchestrator task awaits the chunk futures IN INPUT
/// ORDER and concatenates — input-order results, first-by-input-order errors,
/// cancel-on-drop via the SharedFuture abort handle (the dispatch_worker bridge shape).
async fn task_pmap(&self, args: &[Value], span: Span) -> Result<Value, Control> {
    let input = classify_par_input(&arg(args, 0), "task.pmap", span)?;
    let entry_name = par_callback_name(&arg(args, 1), "task.pmap", span)?;
    let (cap, min_chunk) = par_opts(&arg(args, 2), "task.pmap", span)?;
    let len = input.len();
    if len == 0 {
        return Ok(Value::Future(SharedFuture::resolved(Ok(Value::Array(
            crate::value::ArrayCell::new(Vec::new()),
        )))));
    }
    let plan = chunk_plan(len, cap, min_chunk);

    // Nested (inside an isolate): SAME decomposition, executed inline — venue never
    // changes the value (spec §5.1) and an isolate never blocks on its own pool.
    if crate::worker::pool::in_isolate() {
        return self.par_inline(input, &arg(args, 1), plan, ChunkKind::Map, None, span);
    }

    let slice = crate::worker::build_code_slice_for_interp(self, &entry_name)?;
    let mut chunk_futs = Vec::with_capacity(plan.len());
    for &(start, end) in &plan {
        let (data, job) = input.chunk_payload(start, end);   // Frozen: (whole Shared, start..end)
                                                             // Plain:  (slice copy,   0..end-start)
        let fut = crate::worker::dispatch_worker_job(
            self, slice.clone_for_dispatch(), vec![data], Some(job), span,
        )?;
        chunk_futs.push(fut);
    }

    // Orchestrator: await in INPUT order, concatenate. First panic wins; dropping the
    // remaining futures cancels queued chunks (spec §3.5).
    let fut = SharedFuture::new();
    let cell = fut.cell();
    let handle = tokio::task::spawn_local(async move {
        let mut merged: Vec<Value> = Vec::new();
        let mut futs = chunk_futs.into_iter();
        let result = loop {
            let Some(f) = futs.next() else {
                break Ok(Value::Array(crate::value::ArrayCell::new(merged)));
            };
            match await_worker_future(f).await {            // Value::Future -> get().await
                Ok(Value::Array(a)) => merged.extend(a.borrow().iter().cloned()),
                Ok(other) => break Err(Control::Panic(crate::error::AsError::at(
                    format!("pmap chunk returned a non-array (internal invariant): {}",
                        crate::interp::type_name(&other)), span))),
                Err(e) => break Err(e),                     // remaining futs DROP → cancel
            }
        };
        cell.resolve(result);
    });
    fut.set_abort(handle.abort_handle());
    Ok(Value::Future(fut))
}
```

  Notes for the implementer: `slice.clone_for_dispatch()` — `WorkerCodeSlice` holds
  `Rc<[u8]>`; add a cheap manual clone helper (or `#[derive(Clone)]` if all fields allow) so
  one build serves all chunks. `Plain` chunk payloads are built from the call-time snapshot
  (`elems[start..end].to_vec()` of `Value` clones) — the airlock encode inside
  `dispatch_worker_job` does the sendability gate + copy; a non-sendable element therefore
  panics at dispatch of its chunk, synchronously inside the `pmap` call → the error is
  raised directly (deterministic — chunks dispatch in input order, so the first offending
  chunk by input order raises). Do not hold the input array's borrow across anything —
  snapshot first (the `gather` pattern, `task_mod.rs:117`).
- [ ] **Step 3:** implement `par_inline` (the in-isolate executor): loop the plan, run each
  chunk via the SAME `run_chunk_job` against the current `vm` (the entry is already a global
  — shipped transitively because the enclosing worker body references `f` by name), merge
  identically, return a resolved/spawned future. Map only for now; Reduce lands in 2.3.
- [ ] **Step 4:** edge tests from spec §5.3: order preservation under inverse workloads
  (sleep `(n - i)` ms in `f` — keep total < 1 s), `chunks: 1`, `chunks > len`,
  `minChunk > len`, callback-not-worker-fn panic, non-sendable element field-path panic,
  `?`-in-callback → `nil` element, frozen-element mutation panic + frozen-instance method
  diagnostic (assert the two DIFFERENT messages), plain-instance method works
  (the §3.1 parity battery).
- [ ] **Step 5:** green BOTH configs. Commit —
  `feat(task/par): task.pmap — eager chunk dispatch, input-order merge, inline nesting
  (PAR spec §3.4/§3.5)`.

### Task 2.3: preduce + the final-combine stage

**Files:** modify `src/stdlib/task_mod.rs`.

- [ ] **Step 1 (failing tests):**

```rust
#[tokio::test]
async fn preduce_equals_sequential_for_associative_f() {
    let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
print(await task.preduce([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], add, 0))
print(await task.preduce([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], add, 100, { chunks: 3 }))
"#).await;
    assert_eq!(out, "55\n155\n");   // init participates EXACTLY once (spec §3.3.3)
}

#[tokio::test]
async fn preduce_empty_and_single() {
    let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
print(await task.preduce([], add, 42))
print(await task.preduce([7], add, 1))
"#).await;
    assert_eq!(out, "42\n8\n");     // [] -> init (poolless); [e] -> f(init, e)
}

#[tokio::test]
async fn preduce_nonassociative_is_reproducible_with_pinned_chunks() {
    // (a - b) is non-associative: chunked != sequential, but DETERMINISTIC given the
    // pinned plan — equal on every run and every mode (spec §3.8). Run twice, compare.
    let src = r#"
import * as task from "std/task"
worker fn sub(a, b) { return a - b }
print(await task.preduce([100, 1, 2, 3, 4, 5], sub, 0, { chunks: 2 }))
"#;
    let a = run(src).await;
    let b = run(src).await;
    assert_eq!(a, b);
    // chunks:1 == the sequential fold:
    let seq = run(r#"
import * as task from "std/task"
worker fn sub(a, b) { return a - b }
print(await task.preduce([100, 1, 2, 3, 4, 5], sub, 0, { chunks: 1 }))
"#).await;
    assert_eq!(seq, "85\n");        // ((((((0-100)-1)-2)-3)-4)-5) = -115? NO — seed rule:
    // chunks:1 → ONE chunk seeded with 100, partial = ((((100-1)-2)-3)-4)-5 = 85;
    // final fold = f(init=0, 85) = -85. FIX the expected value to "-85\n" and document:
    // preduce(chunks:1) == f(init, fold(chunk)) — equal to sequential reduce ONLY for
    // associative f, by design (spec §2.1). The test asserts the CONTRACTUAL value.
}
```

  (The implementer must compute the contractual expected values from the §3.3.2/§3.3.3
  rules — seed-with-first per chunk, one final `f(init, p0…)` fold — and assert THOSE; the
  worked example above shows the discipline. For associative `f` they coincide with
  sequential; the non-associative case asserts reproducibility + the documented
  decomposition, never sequential equality.)
- [ ] **Step 2:** implement `task_preduce`: validate (`init` sendability via
  `serialize::check_sendable` up front — fail before any dispatch); empty → resolved
  `init`; plan; in-isolate → `par_inline` Reduce (same decomposition + a local final fold
  via the same element-call helper); else dispatch Reduce chunks, orchestrator collects
  partials in chunk order, then dispatches ONE final Reduce job with plain data
  `[init, p0, .., pk]` (`start=0, end=k+1` — copy path) and resolves with its result.
  Single-chunk inputs still go through the final stage (uniform; 2 dispatches — spec
  §3.3.3).
- [ ] **Step 3:** edge tests: ragged final chunk; non-sendable `init` up-front panic; panic
  inside the combiner during the FINAL stage surfaces as the preduce error; frozen input
  preduce.
- [ ] **Step 4:** green BOTH configs. Commit —
  `feat(task/par): task.preduce — seeded chunk folds + single final-combine dispatch
  (PAR spec §3.3.3)`.

### Task 2.4: error ordering, cancellation, caps

**Files:** `src/stdlib/task_mod.rs` tests (+ any fix they force).

- [ ] **Step 1:** panic-ordering test (spec §3.5): chunk 0 slow-panics (sleep then assert),
  chunk 1 fast-panics; pin `{chunks: 2}`; assert the surfaced message is chunk 0's,
  via `recover`. Run it 5× in-process for flake confidence.
- [ ] **Step 2:** cancellation test: `task.timeout(50, task.pmap(bigInput, slowF))` returns
  the timeout err pair; then a follow-up `pmap` on the same pool succeeds (the
  `InflightGuard` accounting probe). A queued-chunks-never-run probe: `ASCRIPT_WORKERS=1`
  via a `std::env` guard in a serial test (or skip env mutation and assert via chunk-side
  counters — implementer picks the non-flaky design; completion-count assertions must be
  `<= dispatched`, never exact, for the in-flight-runs-to-completion honesty of §3.5).
- [ ] **Step 3:** caps test: `caps.drop` inside the callback is refused (pooled rule §3.6);
  a pre-dropped caller cap is denied inside chunks (mirror the existing
  `pooled_request_install_refuses_drop_and_no_leak` shape at the script level).
- [ ] **Step 4:** nested test: a `worker fn` whose body calls `task.pmap` — deadlock-free,
  identical output to top-level; plus nested non-associative `preduce` with pinned chunks
  equals the top-level value (venue-invariance §5.1).
- [ ] **Step 5:** green BOTH configs (full `cargo test`, not just task_mod — the worker
  battery must stay green). Commit —
  `test(task/par): error ordering, cancellation, caps, nesting batteries (PAR spec §3.5/§3.6)`.

### Task 2.5: Phase 2 independent review

- [ ] Reviewer runs the full suite BOTH configs + clippy BOTH configs; probes edges beyond
  the batteries: pmap over 1-element array; `{chunks: 0}` / `{minChunk: 0}` /
  `{chunks: 1.5}` (validation panics); pmap of a frozen MAP (the `frozen map` panic
  wording); callback that is a `static worker fn` (clear §2.2 panic, not a slice-build
  error leak); `await`-less `let p = task.pmap(...)` then drop (no hang, no zombie);
  re-entrancy (`pmap` whose callback calls another plain `worker fn` — inline nesting
  inside the chunk). Reviewer confirms NO new `pub` surface beyond intent and that
  `dispatch_worker`'s public signature is unchanged.

---

## Phase 3 — Unit C: corpus, negative space, checker, docs

### Task 3.1: examples (Gate 9) + four-mode differential

**Files:** create `examples/data_parallel.as`, `examples/advanced/data_parallel_pipeline.as`.

- [ ] **Step 1:** `examples/data_parallel.as` — intro, order-deterministic, ~40 lines:
  `pmap` squares (print merged array + sum), `preduce` add with non-zero `init`, a
  panicking callback caught by `recover` (print the message prefix), the empty-array edge,
  `?`-in-callback → `nil` element. **Pin `{chunks: 4}` on every `preduce`** so CI machines
  with different core counts emit identical bytes (associative ops only in the corpus —
  spec §5.1).
- [ ] **Step 2:** `examples/advanced/data_parallel_pipeline.as` — production-shaped:
  `shared.freeze` a generated dataset; `pmap` a scoring `worker fn` that returns
  `[value, err]` pairs per element; partition successes/failures locally; `preduce` an
  aggregate; wrap the pipeline in `task.timeout` with a real fallback branch; fully
  error-handled (no naked `!`), deterministic output.
- [ ] **Step 3:** verify all four modes by hand:
  `target/release/ascript run <file>`, `--tree-walker`, `--no-specialize` (or
  `ASCRIPT_ENGINE` per the harness), and `build` → `run file.aso` — byte-identical output.
  Then `cargo test --test vm_differential` BOTH configs (the corpus walk picks the new
  examples up automatically — confirm by grepping the test output for the filenames).
- [ ] **Step 4:** `ascript fmt` both examples — idempotent (Gate 11); `ascript check` —
  zero diagnostics.
- [ ] **Step 5:** Commit — `feat(par): example corpus — intro + production pipeline,
  four-mode verified (Gate 9)`.

### Task 3.2: negative-space pin + Gate-5 sweep

**Files:** create `tests/par_negative_space.rs`.

- [ ] **Step 1:** model on `tests/srv_negative_space.rs` (read it first):

```rust
//! PAR negative space (spec §5.2): PAR is stdlib-only. No .aso change, no new
//! opcode, no new worker-wire tag — the existing worker_serialize fuzz target
//! covers every byte PAR puts on the wire, unchanged.
#[test]
fn aso_format_version_unchanged_by_par() {
    assert_eq!(ascript::vm::aso::ASO_FORMAT_VERSION, 27,
        "PAR must not bump ASO_FORMAT_VERSION; if another spec bumped it, update this \
         pin in ITS branch, never in PAR");
}
#[test]
fn worker_wire_tag_space_unchanged_by_par() {
    // TAG_SHARED = 15 is the last tag; pin via the serializer's public test hook /
    // a round-trip of every kind (grep how srv_negative_space pins it and mirror).
}
#[test]
fn no_new_opcode_for_par() {
    // Pin Op::COUNT / the opcode name table length against the pre-PAR value.
}
```

  (Adapt to the actual visibility — `srv_negative_space.rs` shows the shipped technique for
  each pin; reuse it rather than invent. If `ASO_FORMAT_VERSION` is not 27 on the branch
  because another spec merged first, pin the value found at branch time and note it.)
- [ ] **Step 2:** Gate-5 sweep: `cargo test --test check` BOTH configs — `examples/**` still
  emits zero `type-*` (the new examples included). `cargo test --test treesitter_conformance
  --test frontend_conformance` BOTH configs (must be trivially green — no syntax changed).
- [ ] **Step 3:** fuzz smoke: `cargo +nightly fuzz run worker_serialize -- -runs=100000`
  (or the repo's documented fuzz invocation — check `fuzz/README`/CI config) — green,
  unchanged target.
- [ ] **Step 4:** Commit — `test(par): negative-space pins (ASO/wire/opcodes) + Gate-5 sweep
  (PAR spec §5.2)`.

### Task 3.3: docs (Gate 13)

**Files:** modify `docs/content/stdlib/async.md`, `docs/content/language/workers.md`,
`README.md`.

- [ ] **Step 1:** `async.md` (the `std/task` reference — the brief's `task.md` does not
  exist; this is the verified home): a "Data parallelism" section after `retry`:
  signatures, the chunk-plan formula verbatim (spec §3.3.1), the **`preduce` contract
  blockquote verbatim from spec §3.8** (associativity; reproducibility; `{chunks}` for
  cross-machine pinning), the frozen-vs-plain input table (copy semantics vs frozen-view
  semantics — instances/mutation/cycles rows from §3.1), error/cancel semantics incl. the
  honest in-flight-chunk note (§3.5), the caps note + `run_in_worker` cross-link (§3.6),
  and the break-even guidance citing `bench/DATA_PARALLEL_RESULTS.md` (placeholder number
  filled in by Task 4.1 — leave a `TODO(bench)` marker that Task 4.2 greps for and MUST
  resolve; an unresolved marker fails the final review).
- [ ] **Step 2:** `workers.md`: a "Data parallelism: `task.pmap`" section showing the
  one-line upgrade from the hand-rolled `gather(map(...))` pattern and linking the stdlib
  page + `shared.md`. NO NAV change (no new page) — load the served site once
  (`cd docs && python3 -m http.server`) and click both pages (the orphan gotcha check).
- [ ] **Step 3:** `README.md`: the `std/task` row mentions `pmap`/`preduce` (data
  parallelism across cores).
- [ ] **Step 4:** Commit — `docs(par): std/task pmap/preduce reference + workers-page
  section (Gate 13)`.

### Task 3.4: Phase 3 holistic review

- [ ] Reviewer: runs both examples in all four modes by hand and diffs bytes; runs
  vm_differential BOTH configs; checks the docs render (served site, in-content links
  resolve relative — `](shared)` style); confirms the `preduce` doc contract matches the
  implementation by re-deriving one worked example by hand; greps for `TODO(bench)`
  (must exist exactly once, owned by Phase 4); hunts staleness — `CLAUDE.md` stdlib notes,
  `docs/content/stdlib/overview.md` if it lists task functions.

---

## Phase 4 — Unit D: bench, gates, finish

### Task 4.1: benchmark + report (Gates 16/17/18)

**Files:** create `bench/data_parallel_bench.as`, `bench/run_data_parallel_bench.sh`,
`bench/DATA_PARALLEL_RESULTS.md`.

- [ ] **Step 1:** `data_parallel_bench.as` (model: `bench/workers_bench.as`; in-program
  same-session A/B — both arms in one run, `time.monotonic()` around each):
  (a) **scaling:** sequential `for` map vs `task.pmap` vs hand-rolled
  `gather(array.map(seeds, workerFn))` over 32×400k-iteration LCG chunks (the
  `WORKERS_RESULTS` workload), with a determinism checksum printed; (b) **break-even
  sweep:** per-element work at ~0 / 1k / 10k / 100k LCG iterations, fixed `len`, report
  the crossover; (c) **frozen vs plain input:** `pmap` over 10k/100k/1M-element data both
  ways + the one-time freeze cost; (d) **preduce:** add-reduce scaling + the small-`len`
  `chunks+1` overhead.
- [ ] **Step 2:** `run_data_parallel_bench.sh` (model: `run_workers_bench.sh` +
  `run_shared_heap_bench.sh`): release build; runs the bench at `ASCRIPT_WORKERS` = 1, 2,
  4, 8; wraps each in `/usr/bin/time -l` and extracts max RSS (Gate 18); writes the
  markdown tables into `bench/DATA_PARALLEL_RESULTS.md` with host/date/binary header.
- [ ] **Step 3:** Gate 12/17: `cargo test --test vm_bench` (release, the documented
  invocation in `tests/vm_bench.rs`'s module doc) — the spec/tw geomean ≥2× floor holds
  (PAR touched no engine path; this is the proof, not an assumption). Record the geomean
  in the report.
- [ ] **Step 4:** fill the `TODO(bench)` break-even number into `async.md`; state the honest
  non-goal in the report ("below ~X µs/element, sequential wins — pmap is for coarse
  work").
- [ ] **Step 5:** Commit — `bench(par): scaling + break-even + frozen-vs-copy report,
  RSS, Gate-12 geomean re-run (PAR spec §6)`.

### Task 4.2: meta-docs + status flips

**Files:** modify `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`.

- [ ] **Step 1:** `CLAUDE.md`: extend the Workers/SRV "Larger subsystems" area with a terse
  PAR note (task.pmap/preduce; chunk plan is contractual; frozen=Arc-bump / plain=copy;
  callback = named `worker fn`; `WorkerRequest.chunk` native driver; no ASO/wire change).
  Keep it to the house terseness (gotchas only).
- [ ] **Step 2:** `roadmap.md`: the PAR milestone entry (what shipped, review findings).
  `goal-perf.md`: flip PAR's status to ✅ with a one-line result summary + the measured
  headline (scaling factor + break-even) — and correct any spec-vs-shipped drift in its
  PAR stanza (e.g. the auto-freeze framing → the shipped freeze-or-copy decision).
- [ ] **Step 3:** grep the repo for stale claims: `grep -rn "pmap\|preduce" docs/ README.md
  CLAUDE.md` — every mention consistent with shipped behavior.
- [ ] **Step 4:** Commit — `docs(par): CLAUDE.md + roadmap + goal-perf status (PAR done)`.

### Task 4.3: FINAL holistic review + full gates checklist

The independent holistic reviewer runs EVERYTHING and ticks each box with evidence (paste
command output summaries into the review note):

- [ ] `cargo build` + `cargo build --no-default-features` clean.
- [ ] `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets`
  — zero warnings (Gate 2).
- [ ] `cargo test` AND `cargo test --no-default-features` — full suites green (Gate 3).
- [ ] `cargo test --test vm_differential` BOTH configs — the new examples in the corpus,
  tree-walker == specialized == generic byte-identical; `.aso` mode covered (Gate 1).
- [ ] Examples: `examples/data_parallel.as` + `examples/advanced/data_parallel_pipeline.as`
  run in all four modes byte-identically; fmt-idempotent; `ascript check` clean (Gates 9/11).
- [ ] Gate 5: zero `type-*` on `examples/**` BOTH configs (`cargo test --test check`).
- [ ] `tests/par_negative_space.rs` green: ASO version untouched, wire tags untouched, no
  new opcode (Gate 15 posture: no new engine config → no new kill switch needed; the
  fuzz target `worker_serialize` unchanged and smoked).
- [ ] Bench report committed with same-session numbers, RSS rows, the Gate-12 geomean
  re-run, and the break-even published + cited in docs (Gates 12/16/17/18).
- [ ] Docs: `async.md` + `workers.md` + `README.md` updated; served-site click-through done;
  no NAV orphan; no `TODO(bench)` markers remain (Gate 13).
- [ ] No `await` across a `RefCell` borrow in any new code; `WorkerRequest` `Send`-clean;
  no `unwrap()`/`expect()`/`panic!` reachable from malformed input in new paths (Gate 14
  — reviewer greps the new code specifically).
- [ ] `CLAUDE.md`/`roadmap.md`/`goal-perf.md` updated and accurate.
- [ ] Reviewer adversarial pass (Gate 14): re-probe the §4 failure-mode table row by row
  against the release binary — every row's behavior matches the spec table verbatim
  (messages included). Any divergence: fix in-branch with a failing-test-first guard.
- [ ] Merge: `git checkout main && git merge --no-ff feat/data-parallel` only after every
  box above is ticked.
