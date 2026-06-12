# Record/Replay Flagship (REPLAY) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

**Goal:** Make the shipped, INERT determinism plumbing (`src/det.rs` Record/Replay, virtual
clock, seeded RNG, FFI seam, workflow replay) a user-facing flagship: `ascript run
--record/--replay <trace>` over a versioned binary trace; effectful stdlib I/O recorded at the
`call_stdlib` result boundary (fs/env/io/process.run/os/DNS/http-buffered/workflow.run) with
LOUD Tier-2 refusal of everything unseamed in BOTH modes; `ascript test --record` (per-test
traces auto-saved ONLY on failure) + `test --replay`; and replay-debugging through the shipped
DAP server with `stepBack`/`reverseContinue` via deterministic re-execution (the rr model).
Zero-cost when off; strict replay divergence errors with event index + expected/got.

**Architecture:** Spec: `superpowers/specs/2026-06-12-record-replay-design.md` — **read it
fully before any task**; §0's five code-vs-brief corrections are load-bearing (http results
are native handles → the §2.5 HttpResponse virtualization; `--record` IS a deterministic-mode
run — virtual clock, instant sleeps; `ascript test` runs on the tree-walker; shipped replay
falls through to Record — §7 adds a strict posture for `Origin::CliTrace` only; the §0.5
bare-`time.sleep`-under-Replay bug is fixed first). One hook at `Interp::call_stdlib`
(the caps-gate pattern: a `Cell<bool>` short-circuit), one at `call_native_method` (the
per-handle caps re-check pattern), a complete `replay_class` table with a completeness test
(the `required_cap` pattern), airlock-encoded outcomes (NOT JSON — Int/Float fidelity),
workers refused at all four isolate-creation sites, workflow untouched (its own context swaps
in/out — provenance keeps the hook from firing inside it).

**Tech stack:** Rust, single binary `ascript`. Touched: `src/det.rs`, `src/trace.rs` (new,
core), `src/interp.rs`, `src/stdlib/mod.rs`, `src/stdlib/net_http.rs`, `src/lib.rs`,
`src/main.rs`, `src/dap/{server,launch}.rs`, `tests/{record_replay.rs (new),dap.rs,
determinism.rs,workflow.rs}`, `fuzz/fuzz_targets/trace_roundtrip.rs` (new), `bench/`,
`examples/`, `docs/content/tooling/record-replay.md` (new) + NAV + `docs/content/cli.md`.
No new dependencies (crc32 is hand-rolled or reuses an existing dep — the trace format is the
contract, not the impl; NO serde in `src/det.rs`/`src/trace.rs`: both must build under
`--no-default-features`). **No `ASO_FORMAT_VERSION` bump** (27 — assert unchanged at the end).

**Coordination note (read before Task 1):** WARM Unit C (🏗️, unmerged) introduces a
`DeterminismContext::record_event` append chokepoint in `det.rs`. If WARM has merged by
execution time, route every new append through it; if not, REPLAY's Task 1 may introduce it
and WARM rebases (goal.md reconciliation-6 rule: never two sibling append paths). Check
`git log main -- src/det.rs` first.

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house trailer
  on every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14, goal-perf.md 15–18):** any bug found while
  working — ours or pre-existing (the §0.5 sleep bug is already one), direct or incidental —
  is fixed **in this branch** with a failing-test-first regression guard. No placeholders, no
  silent deferrals; every §11 spec rejection stays rejected; every v2 item is recorded in
  `roadmap.md`.
- Byte-identity is never relaxed: the default (no flag) path is byte-for-byte today's;
  record/replay are byte-identical ACROSS engines (record-on-A/replay-on-B). Workflow + det +
  vm_differential suites pass UNMODIFIED (except the §0.5 failing-first regression addition).
- Clippy clean AND tests green under `--all-targets` and `--no-default-features
  --all-targets` before any "done" claim. Evidence (command output) before assertions.
- No `RefCell`/resource borrow across `.await`. The trace reader parses UNTRUSTED bytes —
  every length bounds-checked, no reachable `unwrap`/`panic!` from hostile input (the P0
  `.aso`-clamp discipline).
- Branch: `feat/record-replay` off `main`. Merge `--no-ff` after holistic review.

---

## File Structure

**New files:**
- `src/trace.rs` — the `ASTRC` container: `TRACE_FORMAT_VERSION`, header struct, binary
  event codec (all `DetEvent` variants), hostile-safe reader, atomic writer. Core
  (no serde, no feature gate; builds under `--no-default-features`).
- `tests/record_replay.rs` — the round-trip / cross-engine / mismatch / refusal batteries
  (spawn-based where real processes are needed, the `tests/cli.rs` precedent).
- `fuzz/fuzz_targets/trace_roundtrip.rs` — hostile trace bytes (the `aso_roundtrip` model).
- `bench/run_replay_bench.sh`, `bench/REPLAY_RESULTS.md`.
- `examples/record_replay.as` (intro: clock/rng/env/fs effects + a comment block on
  record/replay) and `examples/advanced/replay_repro.as` (production-shaped: an
  http+fs+process pipeline with full error handling, built to be recorded).
- `docs/content/tooling/record-replay.md` — the user-facing page (+ NAV entry).

**Modified files:**
- `src/det.rs` — `Origin`/`strict` fields, `DetEvent::{StdlibCall,NativeCall}` +
  `TraceOutcome`, strict-replay divergence plumbing (`pending_divergence`/`take_divergence`),
  the §0.5 sleep fix support.
- `src/interp.rs` — `trace_active: Cell<bool>` + sync in install/take/restore/enter;
  divergence raise helper; the worker-spawn refusal guard (4 sites); per-test event-slicing
  marks in `run_registered_tests_filtered`.
- `src/stdlib/mod.rs` — `replay_class` table + completeness test; the `call_stdlib` hook;
  `trace_stdlib_call`; the §0.5 `time.sleep` Replay consume fix.
- `src/stdlib/net_http.rs` — buffered-response virtualization (vid assignment at record;
  `ResourceState::ReplayVirtual` mint at replay); `{stream:true}`/SSE refusal messages.
- `src/lib.rs` — `run_file_*` record/replay threading; `run_tests_*` recording wiring +
  failure-only trace save.
- `src/main.rs` — `--record/--replay/--seed` on `Run`; `--record/--replay` on `Test`;
  `--replay` on `Dap`; composition-rule CLI errors.
- `src/dap/server.rs` + `src/dap/launch.rs` — replay threading, `supportsStepBack`
  (replay-only), navigation log, `stepBack`/`reverseContinue` re-execution.
- `tests/{dap,determinism,workflow}.rs`, `tests/property.rs` (planted-bug trace guard),
  `docs/content/cli.md`, `docs/assets/app.js` (NAV), `CLAUDE.md`, `goal-perf.md`,
  `superpowers/roadmap.md`.

---

## Task 0: Pin the assumed behaviors (engine-shared seams, tree-walker tests, sleep bug)

The spec leans on three behaviors; pin them FIRST so drift fails loudly. One of them (§0.5)
is expected to FAIL — that failing test is the Gate-14 regression guard Task 1 turns green.

**Files:** `tests/determinism.rs`, `tests/workflow.rs`

- [ ] **Step 1: Engine-shared seam pin.** In `tests/determinism.rs`: run a clock+random+uuid
  program via `run_source_deterministic` (tree-walker) and via a VM-side equivalent (add a
  `#[doc(hidden)] vm_run_source_deterministic(src, seed)` seam to `src/lib.rs` mirroring
  `run_source_deterministic:678` over the `vm_run_source` path — install
  `enter_deterministic(seed)` on the VM's `Interp` before run). Assert byte-identical output
  for the same seed. This is the §10.2 cross-engine foundation pinned BEFORE any new code.
- [ ] **Step 2: Tree-walker test-runner pin.** A comment-anchored test asserting the serial
  `ascript test` path executes on the tree-walker `Interp` (e.g. spawn `ascript test` on a
  fixture whose output distinguishes nothing — instead pin via `run_tests_serial`'s use of
  `load_module`: a unit test that registers a test through `Interp::load_module` and runs
  `run_registered_tests_filtered`, documenting the wiring Task 7 builds on). Keep minimal —
  this is documentation-by-test, not new coverage.
- [ ] **Step 3: The §0.5 failing test.** In `tests/workflow.rs`: a workflow whose body calls
  bare `time.sleep(10)` between two `ctx.call` activities; record (run) it, then `resume` on
  the completed... no — use the CRASH path: run with an activity that panics AFTER the sleep,
  then `resume` with the panic cause removed (env-var-controlled activity). EXPECT (today) a
  false "workflow non-determinism" error at the post-sleep activity — the failing test.
  If it unexpectedly PASSES, stop: re-read `mod.rs:779-789` vs `workflow.rs:562-576` and
  escalate (the spec's ground truth would be wrong).
- [ ] **Step 4: Commit** — `test(det): pin engine-shared seams + tree-walker test path; failing repro for bare-sleep replay desync (REPLAY Task 0, spec §0)` (house trailer; the
  failing test goes in `#[ignore = "REPLAY Task 1 fixes this — see spec §0.5"]` ONLY if CI
  must stay green between tasks on the feature branch — prefer committing it red on the
  branch with a note, un-ignored, fixed in the very next task).
- [ ] **Reviewer checkpoint:** reviewer runs all three on a clean branch checkout, confirms
  Step-1/2 pass and Step-3 fails for the predicted reason (the error message names the
  post-sleep activity), and that NO production code changed.

## Task 1: det.rs core — Origin/strict, StdlibCall/NativeCall events, divergence, sleep fix

**Files:** `src/det.rs`, `src/interp.rs`, `src/stdlib/mod.rs`, `tests/workflow.rs`

- [ ] **Step 1: Write the failing unit tests** (in `det.rs` `mod tests`):
  - `origin_defaults_keep_workflow_semantics`: existing constructors yield
    `Origin::Workflow, strict: false`; exhaustion still falls through to Record; mismatch
    still best-effort recovers (the EXISTING tests already pin this — add only the field
    assertions).
  - `strict_replay_exhaustion_sets_divergence`: a `CliTrace` strict Replay context with an
    empty stream → `clock_now_ms` does NOT switch to Record; `take_divergence()` returns
    `Some(Divergence::Exhausted { at: 0, wanted: "ClockRead" })`.
  - `strict_replay_kind_mismatch_sets_divergence_with_expected_got`: seed a `RandomRead`,
    read the clock → divergence carries `at: 0`, expected/got kind names; the recorded value
    does NOT leak.
  - `stdlib_call_record_then_replay_round_trips`: record two `StdlibCall`s (distinct
    module/func/args_hash, `TraceOutcome::Value(vec![…])`), replay in order → identical
    outcomes; wrong func at cursor → divergence (cursor unmoved, the `replay_actor_call`
    discipline); wrong args_hash → divergence.
  - `stdlib_call_panic_and_propagate_outcomes_replay`: `Panic("boom")` and
    `Propagate(bytes)` round-trip.
  - `native_call_vid_and_method_pinned`: `NativeCall` replay verifies vid AND method.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** per spec §2.3/§7: `Origin { Workflow, CliTrace }`, `strict:
  bool`, `pending_divergence: Option<Divergence>` + `take_divergence()`; new constructors
  `record_trace(seed, start_ms)` / `replay_trace(seed, start_ms, events)` (CliTrace+strict);
  `DetEvent::{StdlibCall, NativeCall}` + `TraceOutcome`; record/replay helper methods
  (`record_stdlib_call`/`replay_stdlib_call`, mirror the FFI helpers' peek-don't-consume
  mismatch discipline); strict gating inside the existing seam readers (`clock_now_ms`,
  `clock_monotonic_ms`, `next_random_f64`, `next_seeded_bytes`: when `strict`, mismatch/
  exhaustion → set `pending_divergence` + return the current best-effort value — the RAISE
  happens at the Interp chokepoint, Task 3). Workflow constructors untouched.
- [ ] **Step 4: Fix §0.5** in `src/stdlib/mod.rs:771-794`: in Replay mode, `time.sleep`
  consumes a recorded `TimerSet` at the cursor (advance cursor + `clock.set_now(wake)`,
  mirroring `workflow.rs:562-576`); on a non-`TimerSet` at cursor: Workflow-origin keeps
  today's lenient append (behavior-compatible), strict CliTrace sets divergence; exhaustion:
  Workflow falls through to record (crash point), strict sets divergence. Un-ignore /
  turn green the Task-0 Step-3 test. Run the FULL workflow + determinism suites — they must
  pass with NO other modifications.
- [ ] **Step 5: Run — expect PASS** both feature configs (det.rs is core; the new code must
  not pull serde — `cargo build --no-default-features` proves it).
- [ ] **Step 6: Commit** — `feat(det): Origin/strict + StdlibCall/NativeCall events + divergence plumbing; fix bare time.sleep replay desync (REPLAY §2.3/§7/§0.5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer confirms: zero behavior change for
  `Origin::Workflow` (diff the det/workflow test files — only additions); the sleep fix's
  failing test is now green and the lenient workflow append path still exists for the
  non-TimerSet workflow case; greps that no new path holds a `RefCell` borrow across await.

## Task 2: The trace container — `src/trace.rs` + fuzz target

**Files:** `src/trace.rs` (new), `src/lib.rs` (module decl), `fuzz/fuzz_targets/
trace_roundtrip.rs` (new), `fuzz/Cargo.toml`, `tests/property.rs`

- [ ] **Step 1: Write the failing tests** (in `trace.rs` `mod tests`): header round-trip
  (run-kind and test-kind, argv, digest, seed); full-event-stream round-trip covering EVERY
  `DetEvent` variant (clock/monotonic/random/bytes/timer/activity/actor/generator/ffi/
  stdlib-call all four `TraceOutcome`s/native-call); truncation at EVERY byte offset of a
  representative trace → clean `Err` naming the record index (loop offsets — the aso fuzz
  discipline); flipped-crc → `Err`; unknown version → the "newer than this binary" error;
  unknown record kind → `Err`; count/length bombs (huge declared len, tiny buffer) → `Err`
  with no large allocation; empty file / magic-only → `Err`. Writer: atomic temp+rename
  (write, crash-simulate via read-only dir on unix — the `write_log_tests` model); flush
  includes the end marker + count.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** per spec §3: `TRACE_FORMAT_VERSION = 1`, `TraceHeader`,
  `write_trace(path, &header, &[DetEvent]) -> Result<(), AsError>` (temp+rename, the
  `workflow.rs:759` pattern), `read_trace(bytes) -> Result<(TraceHeader, Vec<DetEvent>),
  TraceError>` (hostile-safe: every read bounds-checked; per-record crc32 — hand-rolled
  table-driven CRC32 in-module, ~30 lines, no new dep). Pure std; no serde; no feature gate.
- [ ] **Step 4: The fuzz target + planted-bug guard.** `fuzz/fuzz_targets/trace_roundtrip.rs`
  (arbitrary bytes → `read_trace` must return `Ok|Err`, never panic/OOM/hang; module doc per
  the `aso_roundtrip` model; seed corpus written by a `#[test]` helper that records a real
  small trace). In `tests/property.rs`: the planted-bug guard — a curated known-bad buffer
  battery (truncations, crc flips, bombs) asserting clean `Err` IN the normal suite (the
  in-session evidence the fuzz claim rests on). `cd fuzz && cargo build` proves the target
  compiles.
- [ ] **Step 5: Run — expect PASS**; clippy both configs.
- [ ] **Step 6: Commit** — `feat(trace): ASTRC v1 container — hostile-safe codec, atomic writer, fuzz target (REPLAY §3)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs `cargo fuzz run trace_roundtrip` ≥10 min where
  available (findings fixed in-branch with regression tests); verifies the truncation loop
  covers every offset of a trace that includes airlock-byte payloads; confirms no
  `unwrap`/`expect`/indexing-without-check in the reader (grep + read).

## Task 3: The chokepoint — `replay_class` table, `call_stdlib` hook, recorded set

**Files:** `src/stdlib/mod.rs`, `src/interp.rs`, `tests/record_replay.rs` (new, in-process
section)

- [ ] **Step 1: Write the failing tests** (in-process, via `#[doc(hidden)]` seams that
  install a `record_trace`/`replay_trace` context on an `Interp` — the `ffi.rs:1646`
  determinism-test precedent):
  - `classification_is_complete`: every `STD_MODULES` entry yields a class for a probe fn;
    a fabricated module name panics the test (the `required_cap_complete_enumeration`
    model). Feature-gate the assertions like the dispatch arms.
  - `fs_read_records_and_replays_without_fs`: record `fs.read` of a fixture → event stream
    has one `StdlibCall(fs.read)` with `Value` outcome; DELETE the fixture; replay → same
    `[content, nil]` pair, no fs access (file is gone — success proves it).
  - `env_process_os_dns_round_trip`: env.get/set/all, process.run (echo), os.cpuCount,
    net.lookup (gated `net`; skip-with-note offline — better: lookup `localhost`).
  - `int_float_fidelity_through_outcome`: a recorded fn returning `5` and `5.0` (e.g.
    `process.run` exit code int + a constructed pair via fs JSON read… simplest: a direct
    `TraceOutcome::Value` encode/decode unit asserting `Value::Int(5)` ≠ `Value::Float(5.0)`
    round-trip distinctly — the §2.4 verdict made testable).
  - `refused_set_is_loud_in_both_modes`: sqlite.open / net_tcp connect / process.spawn /
    telemetry.init under record AND replay → the Tier-2 message naming the fn + "v2".
  - `mismatch_is_indexed_with_expected_got`: record `fs.read(a)`, replay a program calling
    `fs.read(b)` → error contains `event 0`, both signatures (the §7 format).
  - `non_sendable_result_refused_at_record`: a Recorded fn whose result contains a live
    handle (craft via the seam: classify a test-only fn, or use http pre-Task-4 → expect
    the field-path refusal, then RECLASSIFY in Task 4).
  - `workflow_inside_record_round_trips`: a program calling `workflow.run` under record →
    ONE StdlibCall(workflow.run) in the trace; the workflow's own log written; replay
    returns the result WITHOUT executing (delete the workflow log first; replay must not
    recreate it).
  - `default_path_untouched`: with NO context, `trace_active()` is false and behavior is
    bit-identical (run a stdlib-heavy program with/without the build — covered structurally;
    assert the flag plumbing: install→true, take→false, restore(Some workflow)→false).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:** `trace_active: Cell<bool>` on `Interp` (synced in
  `install_determinism`/`take_determinism`/`restore_determinism`/`enter_deterministic` —
  true iff `Some && origin == CliTrace`); `ReplayClass` + `replay_class(module, func)`
  (feature-independent data, `required_cap`'s shape; per-spec §8 table; `os` whole-module
  Recorded; `time` Seamed; `caps` per-func: reads Harmless, `drop/dropAll` Refused);
  the hook in `call_stdlib` after the caps gate (spec §2.2 verbatim);
  `trace_stdlib_call(module, func, args, shape, span)`: Record → real dispatch → outcome
  encode (airlock; `Err(Panic)` → `Panic(msg)`; `Propagate(v)` → encoded; non-sendable or
  `TAG_SHARED`-side-vector-nonempty → loud record refusal with field path) → append; Replay
  → consume + verify + decode (decode error = corrupt trace error, not a panic). Divergence
  raise helper: `check_divergence(span)` consulted after Seamed dispatches and inside the
  hook. `time.sleep` Recorded shape: record `StdlibCall` void? — NO: sleep stays **Seamed**
  (the virtual-clock TimerSet path, fixed in Task 1) — verify the class table says Seamed
  and replay consumes TimerSet + skips delay (add the assertion to the round-trip test).
- [ ] **Step 4: Run — expect PASS**; full suite + clippy BOTH configs;
  `cargo test --test vm_differential` both configs (zero diff — the hook is flag-gated off).
- [ ] **Step 5: Commit** — `feat(replay): replay_class table + call_stdlib trace hook — record/replay at the result boundary (REPLAY §2.2-2.4/§8)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer probes: (1) the hook sits AFTER the caps gate (a
  denied cap under record errors with the CAP message, not a recorded event); (2)
  `args_hash` divergence fires on a same-fn-different-args replay; (3) no borrow across the
  dispatch await inside `trace_stdlib_call` (the encode happens after the await, borrow
  scopes audited); (4) sabotage: temporarily classify `sqlite` as Harmless and confirm the
  completeness/refusal tests FAIL (then revert).

## Task 4: HTTP virtualization — HttpResponse vid + `call_native_method` hook

**Files:** `src/stdlib/net_http.rs`, `src/interp.rs`, `src/det.rs` (only if a helper is
missing), `tests/record_replay.rs`

- [ ] **Step 1: Write the failing tests** (spawn a local `std/server` from the TEST process
  — the `tests/cli.rs` server-test precedent — serving a JSON + a text route):
  - `http_get_records_handle_and_accessors`: record a program doing `http.get` →
    `resp.status` + `resp.json()`; trace holds `StdlibCall(net_http.get)` with
    `TraceOutcome::Handle{vid:0,…}` + one `NativeCall{vid:0, method:"json"}`. STOP the
    server; replay → identical output (status from materialized fields, body from the
    recorded NativeCall).
  - `two_responses_get_distinct_vids_and_interleave`: two requests, interleaved accessor
    calls — vids 0/1, replay verifies per-vid method order.
  - `http_error_pair_round_trips`: a connection-refused request records `[nil, err]` as a
    plain `Value` outcome (no handle) and replays it.
  - `streaming_and_sse_are_refused`: `{stream:true}` and `http.sse` under record → the
    loud v2 refusal; same under replay.
  - `virtual_handle_method_args_pinned`: `resp.json(Class)` vs `resp.json()` divergence.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** per spec §2.5: reclassify `net_http` request fns as
  `Recorded(HandleShape::HttpResponse)`; at record, after the real call, walk the canonical
  `[handle, err]` pair — `Native(HttpResponse)` → assign vid (a `RefCell<HashMap<u64,u32>>`
  trace-side map on `Interp`, cleared with the context), snapshot `fields` via airlock,
  emit `TraceOutcome::Handle`; at replay, mint `register_resource(NativeKind::HttpResponse,
  decoded_fields, ResourceState::ReplayVirtual { vid })`. Hook at the top of
  `call_native_method` (`interp.rs:4883`, beside the `governing_cap` re-check, gated on
  `trace_active()`): a receiver whose state is `ReplayVirtual` (replay) or whose resource id
  is in the vid map (record) routes through `record/replay_native_call`. `ReplayVirtual` is
  a new `ResourceState` variant — no `Trace`, no OS resource; `close()` recorded like any
  method and replays as `Nil`.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy BOTH configs (everything here is
  `net`-gated except the `ResourceState` variant + vid map, which are core — verify
  `--no-default-features` builds).
- [ ] **Step 5: Commit** — `feat(replay): HttpResponse handle virtualization — vid birth events + NativeCall record/replay (REPLAY §2.5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer probes: a replayed virtual handle never touches
  reqwest (kill all network — e.g. record against the local server, replay with the server
  down AND `--deny net`?? NO — caps compose separately; just server-down + a panicking
  network mock is enough: replay succeeds offline); GC: `ReplayVirtual` holds no traced
  value beyond `fields` (audit `Value::trace` untouched); leak check: replay end drops all
  virtual handles (resources table empty after run).

## Task 5: Worker refusals under trace contexts

**Files:** `src/interp.rs`, `tests/record_replay.rs`

- [ ] **Step 1: Write the failing tests:** under a `CliTrace` context (record AND replay),
  each of: calling a pooled `worker fn`; `WorkerClass.spawn()`; iterating a `worker fn*`;
  `run_in_worker(f, x)` → a clean Tier-2 naming the construct + "not supported under
  --record/--replay (shared-nothing isolates have no trace identity; v2)". Plus: the SAME
  program with NO context runs normally (the guard is trace-gated).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** one guard helper `Interp::refuse_worker_under_trace(what:
  &str, span) -> Result<(), Control>` (checks `trace_active()`), called at: the pooled
  `worker fn` dispatch entry (locate the single shared site that builds the
  `WorkerRequest`), `spawn_actor` (`interp.rs:2299`), the worker-stream spawn
  (`interp.rs:2548`), and `call_run_in_worker` (`interp.rs:6035`). Message modeled on the
  pooled-caps refusal (`caps.rs:894`).
- [ ] **Step 4: Run — expect PASS**; the full workers suites unmodified-green (the guard is
  inert without a CliTrace context).
- [ ] **Step 5: Commit** — `feat(replay): refuse isolate creation under --record/--replay at all four sites (REPLAY §6)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer hunts a FIFTH site (grep `spawn_isolate`/`dispatch(`
  /`WorkerRequest` construction; `task.pipe` over a worker generator; the test-runner's own
  isolates are CLI-side and covered in Task 7) — any uncovered isolate-creation path found
  is fixed here with a test.

## Task 6: CLI `run --record/--replay` + cross-engine differential + corpus round-trip

**Files:** `src/main.rs`, `src/lib.rs`, `tests/record_replay.rs` (spawn section)

- [ ] **Step 1: Write the failing spawn battery** (real binary, tempdirs):
  - `record_then_replay_byte_identical`: a clock/rng/uuid/fs/env program; `run --record
    t.trace p.as` → out1; `run --replay t.trace` → out2 == out1 (stdout+stderr+exit).
  - `replay_offline`: delete the fs fixture between record and replay → replay still out1.
  - `cross_engine_matrix` (THE Gate-1 extension): record on `--tree-walker` → replay on VM
    (default) and with `ASCRIPT_NO_SPECIALIZE` (generic); record on VM → replay on
    `--tree-walker`; `build` the program → record `.as` → replay against the `.aso`. All
    byte-identical.
  - `seed_pins_record`: `--record --seed 7` twice → identical traces (modulo created_ms
    header field — compare event streams) and identical output.
  - `digest_mismatch_is_clean`: edit the program after record → replay errors with the
    "different program" message, exit non-zero.
  - `argv_taken_from_trace_and_conflict_errors`; `record_plus_replay_flag_conflict`;
    `replay_corrupt_trace_clean_error` (truncate the file); `panicking_run_still_writes_
    trace` (a program that panics after effects → trace exists, replay reproduces the panic
    byte-identically); `exit_n_run_writes_trace`.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:** clap surface on `Run` (`--record <FILE>` / `--replay <FILE>` /
  `--seed <N>` requires `--record`; conflict rules per spec §4.1 as clap conflicts or
  explicit errors); `src/lib.rs` threading: a `TraceMode { Off, Record { path, seed },
  Replay { path } }` parameter on `run_file_with_packages` / `run_file_on_vm_with_packages`
  / `run_aso_file` (default `Off` — existing callers unchanged via wrapper fns): Record →
  compute source sha256 + build header + `interp.enter` with `record_trace(seed,
  real_now_ms())`; on completion/panic/exit take the context + `write_trace` (the
  always-flush rule); Replay → `read_trace`, verify version/crc/digest/argv, install
  `replay_trace`, AFTER the run assert full consumption (leftover events → a warning? NO —
  leftover events are fine (the program may end early deterministically)… verify: an
  early-exit replay is legitimate; document: no end-of-stream assertion). `--inspect
  --replay` routes to Task 8; `--inspect --record`, `--profile --record/--replay` are clean
  errors.
- [ ] **Step 4: Corpus round-trip.** Add to `tests/record_replay.rs`: over a curated corpus
  subset (`examples/record_replay.as`, the determinism examples, an FFI libm program where
  the `ffi` feature + fixture exist — reuse the `ffi.rs` test lib), record→replay each,
  assert identity. Keep the subset small + deterministic (no network examples here; http is
  covered by Task 4's local server).
- [ ] **Step 5: Run — expect PASS**; full suite + clippy both configs;
  `vm_differential` untouched/green both configs.
- [ ] **Step 6: Commit** — `feat(cli): ascript run --record/--replay/--seed — header verification, cross-engine matrix, corpus round-trip (REPLAY §4.1/§10)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs the cross-engine matrix by hand once and
  sabotage-tests it (temporarily make the VM replay path skip digest verification → the
  digest test must fail; revert); confirms the trace is written on the panic path by
  killing... no — by the panic test; confirms NO trace file left behind on `--replay`
  (replay never writes).

## Task 7: `ascript test --record` / `--replay` — per-test traces, failure-only save

**Files:** `src/main.rs`, `src/lib.rs`, `src/interp.rs`, `tests/record_replay.rs`

- [ ] **Step 1: Write the failing spawn tests:**
  - `failed_test_saves_trace_passing_saves_nothing`: a 3-test file (pass, fail-with-fs-and-
    rng-effects, pass) under `ascript test --record` → exactly ONE
    `.ascript-traces/<stem>__<slug>.trace`; the "trace saved:" hint line printed; a fully
    green file saves nothing and `.ascript-traces/` is not created.
  - `test_replay_reruns_one_test_deterministically`: `ascript test --replay <trace>` →
    the same failure message byte-for-byte (rng/clock/fs pinned), tally `0 passed 1 failed`,
    exit 1; works after deleting the fs fixture.
  - `replayed_fixed_test_passes`: fix the assertion (the trace pins INPUTS, not the
    assertion) → replay passes — the debug-loop story made testable. (Source-digest rule
    for tests: the header records the digest; a changed test file under `test --replay`
    proceeds with a printed warning instead of the hard error — the whole POINT is editing
    the test/code between replays. Divergence detection remains the guard. Spec §4.3
    sharpened here: implement warn-not-error for kind=test, error for kind=run.)
  - `module_load_prefix_is_sliced` (in-process): file with module-level `random()` + a
    failing test using `random()` → the saved trace replays both draws correctly.
  - `record_parallel_refused`, `record_watch_refused` (CLI errors); `record_with_coverage_
    allowed` (VM path: record a failing test under `--coverage` → trace replays — the
    engine-shared proof inside the test runner).
  - `trace_name_collision_suffixes` (two same-named tests across files with the same stem →
    `~2`).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:** `Test` clap: `--record` (bool) / `--replay <FILE>` + conflict
  rules; `run_tests_serial` (and `run_one_file_with_coverage`) gain the record mode:
  per FILE install `record_trace`; mark the module-load prefix end (`events.len()` after
  `load_module`); `run_registered_tests_filtered` marks `(start, end)` per test (a small
  `Vec<(name, Range)>` returned alongside the summary or recorded on the Interp); after the
  file run, for each FAILURE slice `P ⧺ S_k`, build a kind=test header (file, test name,
  recorded filter, seed) and `write_trace` under `.ascript-traces/` (create dir; slug;
  collision suffix); print hints after the tally. `--replay`: read header → run that file
  with an exact-name internal filter under `replay_trace`; print the normal pass/fail
  output. Document the sibling-state caveat in the docs task.
- [ ] **Step 4: Run — expect PASS**; full suite + clippy both configs.
- [ ] **Step 5: Commit** — `feat(test): --record per-test failure traces + --replay one-test deterministic rerun (REPLAY §4.2-4.3)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer probes: a test that fails during MODULE LOAD (panic
  before any test runs) — a file-level trace with a sensible name + replay works; a test
  whose failure is itself a divergence-class flake (records fine — the trace captures the
  failing draw); 50 failed tests → 50 traces, no quadratic slicing cost (slices are index
  ranges over one Vec); `.ascript-traces/` never created on green runs.

## Task 8: DAP replay-debugging — stepBack/reverseContinue via re-execution

**Files:** `src/dap/server.rs`, `src/dap/launch.rs`, `src/main.rs`, `tests/dap.rs`

- [ ] **Step 1: Write the failing tests** (extend `tests/dap.rs`'s scripted-session
  harness): `initialize` over `ascript dap --replay t.trace` advertises
  `supportsStepBack: true` (and WITHOUT `--replay` stays absent — bitwise-unchanged
  response); a session over a recorded clock/rng/fs program: set a breakpoint, continue,
  read a variable; `stepBack` → `stopped(reason:"step")` at the PREVIOUS stop with
  byte-identical frame/variable snapshots (compare against the forward pass's cached
  values); `reverseContinue` from stop 3 lands on the previous breakpoint stop;
  `stepBack` at the entry stop → a clean error response (nowhere to go); `evaluate` of a
  pure expression at a stop works; `evaluate` calling a Recorded fn (e.g. `fs.read(...)`)
  → `success:false` with the §5.2 refusal message; `run --inspect --replay` end-to-end
  (launch path threading).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement:** thread `replay: Option<PathBuf>` `main.rs → dap::run_server →
  spawn_debuggee → run_program` (read+verify the trace ON the debuggee thread before
  `vm.run`, install `replay_trace`; a bad trace ships the existing Output+Terminated error
  shape, `launch.rs:96-113`). Adapter: `supportsStepBack` in the `initialize` body iff
  replay; a session `nav_log: Vec<NavStep>` (`SetBreakpoints{source,lines}` and
  `Resume(kind)` entries appended as commands are sent); `stepBack`/`reverseContinue`:
  compute the target stop index, run the EXISTING teardown (`teardown_session` +
  `reset_session` — preserving `nav_log`, which must move into the connection-scoped state
  or be carried across the reset explicitly), respawn on the same program+trace, then a
  **driver mode** in the pump/adapter that re-applies the nav_log: emit NO `stopped` events
  for intermediate stops (absorb them, immediately sending the next recorded resume),
  surface only the target stop (reason "step"). Guard re-entrancy (a stepBack while a
  re-execution is in flight → `success:false, "time travel in progress"`).
- [ ] **Step 4: Run — expect PASS**; full `tests/dap.rs` (old sessions untouched) + suite +
  clippy both configs.
- [ ] **Step 5: Commit** — `feat(dap): replay sessions + stepBack/reverseContinue by deterministic re-execution (REPLAY §5)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer runs a MANUAL session (a real DAP client or the
  scripted harness verbose) across ≥3 backsteps and confirms variable values identical each
  visit; probes the absorbed-stops path for stale `pending_verify`/`pending_evaluate` state
  (the SessionState reset discipline, `server.rs:26-59`); confirms a non-replay session's
  protocol bytes are unchanged (capability + event sequences diffed against main).

## Task 9: The determinism audit — finalize the classification, completeness, docs table

**Files:** `src/stdlib/mod.rs` (table cells), `tests/record_replay.rs`, the spec §8 table,
`docs/content/tooling/record-replay.md` (drafted here, finished in Task 11)

- [ ] **Step 1: The sweep.** For EVERY module in `STD_MODULES` (and every fn the audit deems
  ambiguous), verify the §8 classification against the source: confirm `intl` does or does
  not read the system locale (reclassify per evidence); confirm `stream` sources are pure
  (any fs/net-backed source → Refused or Recorded per shape); confirm `compress`/`encoding`/
  `crypto` non-random fns are pure; `bench`/`assert`/`cli`/`color`/`template`/`convert`;
  `log` (stderr sink — Harmless, output not an effect event); `sync`/`time.interval`
  (an `Interval` handle: timer-backed — under replay an interval would real-sleep →
  classify `time.interval/debounce/throttle` Refused v1 with a note, OR virtual-tick if the
  sleep seam already covers it — decide from code, record the decision). Produce the final
  per-function table.
- [ ] **Step 2: Encode it** in `replay_class` + extend the completeness test to assert the
  documented class for every audited row (the table IS the test fixture — a drift between
  docs and code fails here).
- [ ] **Step 3: Update spec §8** with the finalized table (status header notes the audit
  date) and draft the docs-page version.
- [ ] **Step 4: Run — expect PASS** (any reclassification ripples into Task-3 tests —
  update those WITH evidence, never silently).
- [ ] **Step 5: Commit** — `feat(replay): determinism audit — finalized replay_class coverage table + drift test (REPLAY §8)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer independently spot-audits 5 modules (incl. `intl`
  and `time.interval`) against the source and confirms the table; fabricates an unclassified
  module in a scratch build to confirm the completeness test trips.

## Task 10: Performance — zero-cost gate + record/replay A/B report

**Files:** `bench/run_replay_bench.sh`, `bench/REPLAY_RESULTS.md`, `tests/vm_bench.rs`
(re-run only)

- [ ] **Step 1:** `bench/run_replay_bench.sh` (same-session protocol, Gate 16): (i)
  **zero-cost-when-off** — the existing bench corpus on this branch vs main-merge-base,
  interleaved ×5, expect ≈1.0× (the only delta is the `Cell` check); (ii) **record
  overhead** — an effect-heavy workload (fs reads/writes loop + process.run + the local-
  server http loop) plain vs `--record`, report the per-call overhead honestly + trace size
  + peak RSS (`/usr/bin/time -l`, Gate 18 — the in-memory event buffer is the thing to
  watch); (iii) **replay speed** — the same workload's record wall-time vs replay wall-time
  (the headline: replay does no I/O and sleeps are virtual — include a `time.sleep`-heavy
  case to show it).
- [ ] **Step 2:** Re-run the standing gates: `cargo test --release --test vm_bench --
  --ignored --nocapture` — the DBG zero-cost gate (instrument==None ≈ armed-idle) and the
  spec/tw geomean ≥2× floor (Gate 17 — REPLAY touches `call_stdlib`, the call path's
  neighbor; prove no tax).
- [ ] **Step 3:** Write `bench/REPLAY_RESULTS.md` (machine/date/methodology header; the
  three tables; honest framing — no number promised in the spec, every number measured).
- [ ] **Step 4: Commit** — `bench(replay): zero-cost A/B + record overhead + replay-speed report (REPLAY §9, Gates 16-18)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer re-runs the script; numbers within noise of the
  report; if the off-path delta exceeds noise, the `Cell` home is wrong — fix it here (the
  Gate-12 rule: fix the home, never relax the gate).

## Task 11: Examples, docs page + NAV, CLI/test docs, status updates

**Files:** `examples/record_replay.as`, `examples/advanced/replay_repro.as`,
`docs/content/tooling/record-replay.md`, `docs/assets/app.js`, `docs/content/cli.md`,
`CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md`, spec status header

- [ ] **Step 1: Examples.** `examples/record_replay.as` — intro: clock/rng/env/fs effects,
  comments explaining record/replay + the deterministic-mode contract (virtual clock,
  instant sleeps, seeded RNG); runs standalone (no flags) as an ordinary corpus program
  (four-mode tested, fmt-idempotent). `examples/advanced/replay_repro.as` —
  production-shaped: an fs+process(+http-if-reachable, error-handled offline) pipeline with
  full error handling, written as the canonical "record a failure, replay it offline" demo;
  verify with `target/release/ascript run` (all modes) AND an actual `--record`/`--replay`
  cycle by hand.
- [ ] **Step 2: The docs page.** `docs/content/tooling/record-replay.md`: what a trace is;
  `run --record/--replay/--seed`; the deterministic-mode contract (§0.2 — stated up front);
  `test --record`/`--replay` + `.ascript-traces/` (+ "gitignore it") + the sibling-state
  caveat; replay-debugging + stepBack (+ the honest re-execution cost note); the §8
  coverage table (replays / refused / pure); the divergence error explained; the
  concurrency residual (task interleaving not pinned — detected, not silently wrong);
  v2 items (streaming/workers/checkpointing). **NAV:** add
  `['tooling/record-replay', 'Record & replay']` to the tooling block in
  `docs/assets/app.js` (the orphan-page tripwire — sidebar AND cmd-K derive from NAV);
  serve the site (`cd docs && python3 -m http.server`) and click through.
- [ ] **Step 3: CLI/test docs.** `docs/content/cli.md`: the new `run` flags, the new `test`
  flags, the composition rules (what refuses what), `ascript dap --replay`. Cross-link the
  tooling page. Check `docs/content/tooling/debugging-profiling.md` for a stepBack mention
  + link.
- [ ] **Step 4: Status.** `CLAUDE.md`: a REPLAY paragraph (the chokepoint hook + Cell gate,
  the classification table + completeness test, airlock outcomes, HttpResponse-only
  virtualization, strict CliTrace vs lenient Workflow origins, worker refusal, the trace
  format + fuzz target, `.ascript-traces/`, DAP stepBack-by-re-execution).
  `goal-perf.md`: REPLAY 🏗️ → ✅ in the table with the headline (replay-speed) number.
  `superpowers/roadmap.md`: the milestone entry + recorded v2 follow-ups (streaming/SSE/WS
  + general handle virtualization; per-isolate worker traces; replay checkpointing;
  task-identity event tags; `--deterministic` alias; `--profile`/`--inspect` ×record
  matrix) — each owner-noted, none silent. Spec status header →
  `Implemented (merged <sha>)` with deltas-from-spec recorded (e.g. the Task-7 test-digest
  warn-not-error sharpening).
- [ ] **Step 5: Commit** — `docs(replay): record-replay page + NAV + CLI/test docs + examples + status (REPLAY §4/§5/§8, Gate 13)` (house trailer).
- [ ] **Reviewer checkpoint:** reviewer serves the docs site and uses cmd-K to find the new
  page (the NAV tripwire, verified not assumed); runs both examples on all four modes;
  confirms `docs/content/cli.md` lists every new flag (cross-checked against `--help`
  output).

## Task 12: Holistic review, FINAL GATES, merge

**Files:** none new

- [ ] **Step 1: FINAL GATES CHECKLIST** (every box requires pasted command output in the
  task log — evidence before assertions):
  - [ ] `cargo clippy --all-targets` clean AND `cargo clippy --no-default-features
        --all-targets` clean.
  - [ ] `cargo test` green AND `cargo test --no-default-features` green.
  - [ ] `cargo test --test vm_differential` green BOTH configs, zero file diff vs main
        (REPLAY adds no differential mode — the flag-gated feature is off there; the
        cross-engine record/replay matrix in `tests/record_replay.rs` is the Gate-1
        extension and is green).
  - [ ] `cargo test --test record_replay --test dap --test determinism --test workflow`
        green; `cd fuzz && cargo build` green; a ≥10-min `cargo fuzz run trace_roundtrip`
        session where available, no findings (or fixed in-branch with regression tests).
  - [ ] `cargo test --release --test vm_bench -- --ignored --nocapture`: spec/tw geomean
        ≥2× AND the DBG zero-cost gate green (Gate 17).
  - [ ] `bench/REPLAY_RESULTS.md` complete (off-path ≈1.0×, record overhead + RSS, replay
        speed), same-session methodology stated.
  - [ ] `ASO_FORMAT_VERSION` unchanged (27): `git diff main -- src/vm/aso.rs` empty.
  - [ ] Workflow/det semantics for `Origin::Workflow` unchanged: `git diff main --
        tests/workflow.rs tests/determinism.rs` shows ONLY additions (the §0.5 regression
        test + pins).
  - [ ] No new `unwrap`/`expect`/`panic!` reachable from untrusted input (trace bytes) —
        reviewer grep + justification list.
  - [ ] Examples run on all four modes; fmt-idempotent; docs site serves; NAV entry
        reachable.
- [ ] **Step 2: Holistic review** — a fresh reviewer subagent reviews the WHOLE branch diff
  against the spec: the §8 table vs `replay_class` vs the docs table (three-way
  consistency); the §2.2 hook placement vs the caps gate ordering; every refusal message
  actionable; cross-feature interactions probed by RUNNING them — record under `--sandbox`
  (caps deny beats recording), record a workflow program, record under `--tree-walker`,
  replay a `test` trace whose file moved, a DAP replay session after a `stepBack` storm
  (10×), `--coverage --record` together; and latent bugs in neighbors (`call_stdlib`'s
  typed-parse early returns BEFORE the hook — verify json.parse with a Class arg under
  record is classified/handled; `call_native_method`'s other early paths). All findings
  fixed in-branch with regression tests before merge.
- [ ] **Step 3: Merge** — `git checkout main && git merge --no-ff feat/record-replay` with
  a summary merge message (house trailer). Update `goal-perf.md` status post-merge.

---

## Standing rules for every task (repeated so no subagent misses them)

1. **Bug-fix discipline:** any defect encountered — REPLAY code or pre-existing (§0.5 is the
   known one), surfaced directly or incidentally — gets a failing-test-first fix **in this
   branch**, logged with its commit. A known bug left in the tree is campaign-blocking.
2. **Never a silent wrong replay.** Every gap is a loud Tier-2 refusal or an indexed
   divergence error (the FFI §7B precedent). Lenient fall-through exists ONLY for
   `Origin::Workflow` (the shipped crash-point semantics) — never for `CliTrace`.
3. **Both feature configs, every time.** `det.rs`/`trace.rs`/the classification table/the
   refusal guard are CORE (must build `--no-default-features`); fs/http/process recording is
   gated exactly like its module's dispatch arm.
4. **No borrow across await** (encode outcomes AFTER the dispatch await, in a sync scope);
   **no env reads in parallel tests** (thread flags through `#[doc(hidden)]` seams).
5. **Untrusted bytes are hostile:** the trace file is attacker-writable; the reader
   bounds-checks everything, never panics, and fails to a clean Tier-1 error.
6. **Honest numbers + honest docs:** the deterministic-mode contract (virtual clock, instant
   sleeps, seeded RNG), the re-execution cost of stepBack, the concurrency residual, and the
   sibling-test caveat are stated in user docs verbatim-in-spirit from the spec — never
   soft-pedaled. Every performance claim traces to `bench/REPLAY_RESULTS.md`.
