# `std/resilience` for Backend Hosting (RESIL) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship `std/resilience` (feature `resilience`, default-on): circuit breaker, token-
bucket rate limiter (plain + keyed over a `std/lru` bucket store), bulkhead + load shedding
over `sync.semaphore`, retry v2 (compatible `task.retry` extension + a stateful policy with
retry budgets), fallback, singleflight over `SharedFuture`, stampede-protected memoize,
**deadline/trace task-locals** (the one runtime seam — `tokio::task_local!`, copy-on-spawn at
every async spawn site in BOTH engines, zero-cost when unused), deadline-aware I/O consult
sites (http/postgres/redis/sqlite), a Prometheus `/metrics` handler over a per-isolate
registry, health handlers, and the `resilience.handler` HTTP wrapper — all policies as
`__resil`-tagged Objects with a call-site method hook mirroring `std/schema`'s exactly, all
four modes byte-identical, no grammar/opcode/`.aso` change.

**Spec:** `superpowers/specs/2026-06-12-resilience-stdlib-design.md`. **Read it first and in
full** — §2 (tagged-object + hook model), §3 (per-policy semantics incl. the breaker state
machine §3.1.2 and the retry-v1 compatibility table §3.4), §5 (task-local placement decision +
spawn-site inventory + the two Gate-14 pre-existing fixes), §6 (registry decision +
handler mechanics), §8 (det routing), §10 (the test matrix — every row becomes a test).
Section references (§) below are into it.

**Before writing any code, read these files end to end** (line refs were verified 2026-06-12 —
**re-grep every symbol before editing**, names are the anchors):
- `src/stdlib/schema.rs` (`make_schema:131`, `SCHEMA_KINDS:165`, `is_schema_value:176`,
  `is_schema_method:197`) and BOTH hook sites: `src/interp.rs:4122-4216`,
  `src/vm/run.rs:4838-4925`
- `src/stdlib/task_mod.rs` (whole file — retry v1 contract `:203-318`, jitter exemption
  `:444-472`, the three retry tests `:484-549`)
- `src/task.rs` (whole file — ResultCell/SharedFuture, multi-awaiter, cancel-on-drop)
- `src/stdlib/sync.rs` (`Semaphore:51`, `sync_acquire:483`, `sync_with_permit:563`)
- `src/stdlib/lru.rs` (whole file)
- `src/interp.rs:55-113` (TELEMETRY_CURRENT + scope/capture/root), `:1376-1406` (det seams),
  `:5306-5341`, `:5898-5925`, `:5975-6005` (tree-walker spawn sites), `:1690-2110`
  (telemetry soft hook + instruments)
- `src/vm/run.rs:1728-1756`, `:5704-5717` (VM spawn sites — currently missing
  `telemetry_scope`, spec §1 fix #1)
- `src/stdlib/http_server.rs` (`register_route:1036`, method dispatch `:1068`,
  `value_to_response:835`, `run_chain:2042`)
- `src/stdlib/net_http.rs:550-600` (timeout opts → reqwest builder)
- `src/stdlib/mod.rs` (`std_module_exports:114`, `STD_MODULES:221`, `required_cap:325`,
  completeness tests `:991`/`:1086`)
- `src/det.rs` + `src/lib.rs:678` (`run_source_deterministic`)
- `src/value.rs:492` (`NativeKind` — adding `Resilience`; check `governing_cap` impl and the
  worker serializer's Native handling before assuming)

**Architecture:** Phase 1 (Unit A — module skeleton + hook + breaker). Phase 2 (Unit B —
limiter/keyed/bulkhead/retry-v2/fallback). Phase 3 (Unit C — singleflight + memoize). Phase 4
(Unit D — task-locals + deadline + I/O consult sites + zero-cost bench + the two Gate-14
fixes). Phase 5 (Unit E — registry/metrics/health/handler). Phase 6 (Unit F — examples,
negative space, docs, bench, meta, final holistic).

**Tech stack:** Rust; the `!Send` per-isolate runtime (never add `Send` bounds; **never hold a
`RefCell`/ObjectCell borrow across `.await`** — policy state is mutated in synchronous
sections around awaits, the `sync.rs` discipline); tokio `current_thread` + `LocalSet`; tests
via `cargo test` in BOTH feature configs; `tests/vm_differential.rs`; `/usr/bin/time -l` for
RSS.

**Hard rules carried from the spec:**
- **No grammar/opcode/`Value`-kind/`.aso` change.** `ASO_FORMAT_VERSION` stays 27
  (`src/vm/aso.rs:167`) — pinned by `tests/resil_negative_space.rs`. If another spec bumped it
  before branch time, pin the value found and note it.
- **The hook is call-position only** and mirrored in both engines from ONE
  `call_resilience_method`; bare member reads still read fields (§2.3).
- **`task.retry` v1 behavior preserved bit-for-bit** — the three shipped tests in
  `task_mod.rs` must pass UNCHANGED (§3.4).
- **Every observable time read routes through `clock_now_ms`/`clock_monotonic_ms`**
  (`interp.rs:1382/1393`); sleep durations may be real (the `task_mod.rs:444-452` timing-only
  exemption) (§8).
- **Policy rejections are Tier-1 `[nil, {message, code}]` pairs with the §2.4 stable codes;
  misuse is Tier-2.** Panics from user fns are recorded where relevant but NEVER swallowed
  (except `fallback`, which documents the consume).
- **Every policy object carries the `__local` Native marker** (§2.2) so the airlock refuses it
  loudly.
- `resilience = []` feature, in `default`; the module builds with `--no-default-features
  --features resilience` (its substrates are core); log/telemetry mirrors are `#[cfg]`'d
  inside.

**Binding execution standards (production-grade mandate):** any bug found while working —
ours or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first
regression guard, never stepped around (goal.md Gate 14; two are already known: spec §1 fixes
#1/#2). No placeholders, no silent deferrals. Branch: `feat/resilience-stdlib` off `main`.
Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/stdlib/resilience.rs` — the module (constructors, `call_resilience_method`, policy
  engines, registry, handlers) + inline unit tests.
- `tests/resil_negative_space.rs` — ASO pin, no-new-opcode pin, hook-order pin, retry-v1
  contract pins.
- `examples/resilience.as` + `examples/advanced/resilient_gateway.as`.
- `docs/content/stdlib/resilience.md`.
- `bench/resilience_bench.as` (zero-cost A/B workload half lives in `tests/vm_bench.rs` —
  see Task 4.5), `bench/RESILIENCE_RESULTS.md`.

**Modified files:**
- `Cargo.toml` (`resilience` feature, default list).
- `src/stdlib/mod.rs` (exports arm, `STD_MODULES`, call routing, `required_cap` completeness).
- `src/interp.rs` (hook branch; `TASK_LOCALS` + scope/capture helpers; `deadline_remaining_ms`;
  spawn-site wraps; `ambient_root_scope`).
- `src/vm/run.rs` (hook branch in `dispatch_method`; spawn-site wraps incl. the telemetry
  fix #1).
- `src/value.rs` (`NativeKind::Resilience` + its `governing_cap`/display arms).
- `src/stdlib/sync.rs` (visibility: `pub(crate)` semaphore acquire/release internals for the
  bulkhead — no behavior change).
- `src/stdlib/task_mod.rs` (retry v2 additive keys; shared retry engine callable from the
  policy object).
- `src/stdlib/net_http.rs`, `src/stdlib/postgres.rs`, `src/stdlib/redis.rs`,
  `src/stdlib/sqlite.rs` (deadline consult sites, §5.4).
- `src/stdlib/log.rs`/log record builder (traceId field, §5.5);
  `src/stdlib/telemetry/mod.rs` (doc-comment fix #2).
- `src/check/std_arity.rs` — resilience entries **iff** the table curates comparable modules
  (verify how `std/schema`/`std/task` are handled; if absent, add nothing and note it).
- `docs/assets/app.js` (`NAV` — the orphan gotcha), `docs/content/language/workers.md`,
  `docs/content/stdlib/async.md`, `README.md`.
- `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md` — final task.

---

## Phase 0 — Preflight: branch + semantic pins

### Task 0.1: branch + pin the inherited semantics RESIL composes

**Files:** `tests/resil_negative_space.rs` (created now with the pins; grows later).

- [ ] **Step 1:** `git checkout -b feat/resilience-stdlib main`. `cargo build --release`
  clean. Record `ASO_FORMAT_VERSION`'s current value from `src/vm/aso.rs`.
- [ ] **Step 2:** Write PASSING pin tests for shipped behavior the design leans on (if any
  fails, STOP — the spec's ground truth moved):

```rust
//! RESIL negative space + phase-0 pins (spec §1, §10).
#[test]
fn aso_format_version_unchanged_by_resil() {
    assert_eq!(ascript::vm::aso::ASO_FORMAT_VERSION, 27,
        "RESIL must not bump ASO_FORMAT_VERSION; if another spec bumped it first, \
         update this pin in ITS branch and re-pin here at rebase");
}
// task.retry v1 contract pins (spec §3.4 — these MUST stay green untouched through Phase 2):
// run the three shipped scenarios end-to-end via the public entry (mirror
// task_mod.rs::tests retry_succeeds_on_third_attempt / retry_exhausts_and_reraises /
// retry_does_not_retry_error_pairs as integration-level copies so a task_mod edit
// cannot silently weaken them).
```

  Also pin (as `#[tokio::test]` via `run_source`): a `SharedFuture` awaited by two awaiters
  delivers the same value, and a panicking async fn awaited twice panics both times (the §3.6
  substrate); `sync.semaphore` acquire/release round-trip; `lru` eviction (one-liner each —
  these are the composition substrates).
- [ ] **Step 3:** `cargo test --test resil_negative_space` green in BOTH configs. Commit —
  `test(resil): phase-0 pins — ASO, retry-v1 contract, SharedFuture/semaphore/lru substrates
  (spec §1)`.

### Task 0.2: Phase 0 review

- [ ] Independent reviewer: pins assert live behavior (translate two to `.as` files and run
  against the release binary); confirm no source files changed besides the new test; confirm
  the recorded ASO value matches `src/vm/aso.rs`.

---

## Phase 1 — Unit A: module skeleton, the call-site hook, the circuit breaker

### Task 1.1: feature + module registration + `NativeKind::Resilience`

**Files:** `Cargo.toml`, `src/stdlib/mod.rs`, `src/stdlib/resilience.rs` (new),
`src/value.rs`.

- [ ] **Step 1 (failing test):** in `src/stdlib/resilience.rs` `#[cfg(test)]`:

```rust
#[tokio::test]
async fn module_imports_and_constructs_breaker() {
    let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 2,
                            cooldownMs: 1000, halfOpenMax: 1})
print(b.__resil)        // bare member read — reads the FIELD (call-position-only contract)
print(b.failureRate)
"#).await;
    assert_eq!(out, "breaker\n0.5\n");
}
```

- [ ] **Step 2:** `Cargo.toml`: `resilience = []` under `[features]`; add `"resilience"` to
  `default`. `mod.rs`: `#[cfg(feature = "resilience")] pub mod resilience;`, the
  `std_module_exports` arm, the `call` routing arm (`"resilience" => self.call_resilience(
  func, args, span).await` — grep how `call_task` is routed and mirror), `STD_MODULES` entry,
  and the `required_cap` completeness enumeration (verdict `None`; run the completeness tests
  at `mod.rs:991`/`:1086` — they MUST be updated in the same commit or they fail, which is the
  point).
- [ ] **Step 3:** `src/value.rs`: `NativeKind::Resilience` + every exhaustive-match arm the
  compiler demands (display/type-name/`governing_cap` → `None`/worker-serializer non-sendable
  path — verify Native is already non-sendable generically; if there is a per-kind match, add
  the arm). Grep `NativeKind::` across `src/` and let the compiler find the rest.
- [ ] **Step 4:** implement `exports()` + `breaker(opts)` construction only (validation per
  spec §3.1: defaults `failureRate: 0.5`, `window: 20`, `minCalls: 10`, `cooldownMs: 30000`,
  `halfOpenMax: 3`, `name: "default"`; Tier-2 on out-of-range — `failureRate ∈ (0,1]`,
  positive ints elsewhere). The tagged object carries config fields + state fields
  (`__state: "closed"`, `__ring: []`, `__ringIdx: 0`, `__calls/__failures/__rejected: 0`,
  `__openedAtMs: nil`, `__halfOpenInFlight: 0`, `__halfOpenSuccesses: 0`) + the `__local`
  marker (a `Value::Native` with `kind: Resilience`, `id: u64::MAX`, empty fields — the
  `noop_handle` pattern, `telemetry/mod.rs:113`).
- [ ] **Step 5:** test green in BOTH configs (`cargo test --no-default-features --features
  resilience` must also build — run it). Commit — `feat(resil): module skeleton, feature,
  registration, breaker constructor (spec §2.1/§2.2)`.

### Task 1.2: the call-site hook in BOTH engines (failing test first)

**Files:** `src/stdlib/resilience.rs`, `src/interp.rs`, `src/vm/run.rs`.

- [ ] **Step 1 (failing tests):**

```rust
#[tokio::test]
async fn hook_routes_method_calls_and_only_calls() {
    let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 2})
print(b.state())          // hook → call_resilience_method
print(type(b.state))      // bare read of a NON-field name → nil read per Object semantics?
"#).await;
    // GROUND the second line against today's Object member-read semantics for a missing
    // key BEFORE asserting (run `print(type(({}).x))` on main) — assert exactly that.
    assert!(out.starts_with("closed\n"));
}

#[tokio::test]
async fn hook_unknown_method_is_tier2_and_optmember_not_routed() {
    // b.frobnicate() → Tier-2 panic "breaker policy has no method 'frobnicate'".
    // b?.state() must NOT route (OptMember excluded — schema parity); ground its
    // baseline behavior on main first, then assert it is UNCHANGED.
}
```

- [ ] **Step 2:** implement `is_resilience_value` (`__resil` ∈ `RESIL_KINDS`; include all six
  kinds now), `is_resilience_method` (the §2.3 union set), and the
  `Interp::call_resilience_method` dispatcher (kind×name validation; only `state`/`stats`/
  `reset` implemented so far, others → Tier-2 `not yet wired` is FORBIDDEN — instead route to
  `unimplemented_policy_method` returning the same kind×name Tier-2 panic until the kind
  lands; by Phase 3 every kind×name pair in the set is live and a sweep test proves it).
- [ ] **Step 3:** wire the tree-walker branch (after schema at `interp.rs:4140-4151`, before
  workflow; `#[cfg(feature = "resilience")]`, same receiver-then-args eval order, same
  comment style) and the VM branch (after schema in `dispatch_method`, `vm/run.rs:4873-4882`;
  same cfg). The two branches call the SAME `call_resilience_method`.
- [ ] **Step 4:** four-mode check by hand on a scratch `.as` (run + `--tree-walker` +
  `ASCRIPT_NO_SPECIALIZE`/`--no-specialize` + build→`.aso`): identical bytes. Tests green
  BOTH configs. Commit — `feat(resil): call-site method hook, both engines, call-position
  only (spec §2.3)`.

### Task 1.3: the breaker state machine + `call`

**Files:** `src/stdlib/resilience.rs`.

- [ ] **Step 1 (failing tests, the §10 breaker rows):**

```rust
#[tokio::test]
async fn breaker_opens_at_threshold_and_rejects_with_code() {
    let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 4, failureRate: 0.5, cooldownMs: 60000})
fn bad() { return [nil, {message: "down"}] }
fn good() { return 1 }
b.call(good); b.call(good); b.call(bad); b.call(bad)   // 2/4 = 0.5 ≥ threshold → open
print(b.state())
let [v, err] = b.call(good)
print(v); print(err.code); print(b.stats().rejected)
"#).await;
    assert_eq!(out, "open\nnil\nbreaker-open\n1\n");
}
// + minCalls-1 never opens (window-boundary exactness);
// + rate just BELOW threshold stays closed (e.g. 1/4 with 0.5);
// + panic counts as failure AND re-raises (recover around b.call);
// + Propagate passes through unrecorded;
// + rejected calls don't enter the window (open → many rejections → reset cooldown via
//   virtual clock → halfOpen probe sees the ORIGINAL window stats cleared per §3.1.2);
// + reset() returns to closed and clears the ring.
```

- [ ] **Step 2 (cooldown + half-open under the virtual clock; failing first):** use
  `run_source_deterministic(src, seed)` (`lib.rs:678`) — but note it starts a virtual clock
  that only advances via timer/sleep seams; GROUND how `time.sleep` advances `VirtualClock`
  (read `src/stdlib/time_timers.rs` + `det.rs:171-189`) and write the cooldown test
  accordingly (sleep past `cooldownMs` → next call admits a probe → `state() == "halfOpen"`;
  probe success ×`halfOpenMax` → closed; probe failure → open with FRESH `__openedAtMs`).
  Half-open probe-budget race: `halfOpenMax: 1`, two CONCURRENT async probes (spawn both
  before awaiting) → exactly one admitted, one `breaker-open`.
- [ ] **Step 3:** implement per spec §3.1.2/§3.1.3: outcome classification shares a
  `result_pair_err(v) -> Option<Value>` helper (the 2-element-pair shape — grep how `?`/
  `ExprKind::Try` detects pairs and REUSE that exact predicate; if it is inline, extract it
  to a shared `pub(crate)` fn in `interp.rs` so both stay identical); all state mutation in
  synchronous sections (read-modify-write under short ObjectCell borrows; the `fn()` await
  happens with NO borrow held); `__halfOpenInFlight` increment/decrement on ALL exits
  (panic included). Time reads: `self.clock_monotonic_ms(real_now())` — grep how
  `time.monotonic` computes its real value and reuse.
- [ ] **Step 4:** green BOTH configs. Commit — `feat(resil): breaker state machine —
  count-window, cooldown, half-open probe budget (spec §3.1)`.

### Task 1.4: Phase 1 independent review

- [ ] Reviewer runs full suite + clippy BOTH configs; probes: `b.call()` with no arg /
  non-callable (Tier-2 wording); `breaker({failureRate: 0})` / `{failureRate: 1.5}` /
  `{window: 0}` rejected; bare `b.call` member read (returns nil-or-field per grounded
  semantics, never a bound method); a user object `{__resil: "bogus"}` is NOT hijacked
  (`.state()` falls through to normal member-call error); hook parity — the same scratch
  program byte-identical in all four modes; `__local` present on the policy
  (`print(type(b.__local))` → `"native"`); state mutation visible across calls in the SAME
  policy value but independent across two constructed policies.

---

## Phase 2 — Unit B: limiter, keyed limiter, bulkhead, retry v2, fallback

### Task 2.1: token-bucket limiter (plain)

**Files:** `src/stdlib/resilience.rs`.

- [ ] **Step 1 (failing tests):** capacity-exhaustion determinism
  (`limiter({capacity: 2, refillPerSec: 0.001})`: two `tryAcquire()` true, third false);
  `tryAcquire(5)` atomicity (5 available → true and exactly 5 gone; 4 available → false and
  NONE gone); refill math under the virtual clock (deterministic run: capacity 1,
  refillPerSec 1000, sleep 2ms → tryAcquire true); validation panics (capacity 0, negative
  refill, non-number).
- [ ] **Step 2:** implement per §3.2.1 (state `__tokens`/`__lastMs` floats; refill formula
  verbatim; `acquire` deficit-sleep loop — compute deficit under a short borrow, drop it,
  `tokio::time::sleep`, re-loop; nothing borrowed across the await). `acquire` integration
  test: capacity 1, refillPerSec 500 → two sequential `await lim.acquire()` complete (the
  second after ~2ms) — assert completion, not timing.
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): token-bucket limiter —
  acquire/tryAcquire(n), det-routed refill (spec §3.2.1)`.

### Task 2.2: keyed limiter over `std/lru`

- [ ] **Step 1 (failing tests):** per-key isolation (key A exhausted, key B unaffected);
  **eviction documented behavior** (`maxKeys: 2`, exhaust key A, touch B then C → A evicted →
  A's next `tryAcquire` succeeds on a FULL bucket; eviction counter incremented — read it via
  `stats()`); non-string key → Tier-2.
- [ ] **Step 2:** implement per §3.2.2: `__store` = a real lru handle (call into
  `call_lru_new`'s registration path — make the needed constructor `pub(crate)` rather than
  routing through the string dispatcher); bucket entries are `{tokens, lastMs}` Objects;
  every touch goes through lru `get`/`set` so recency/eviction is the shipped machinery.
  `stats()` exposes `{keys, evictions}` (evictions tracked by an int field — lru doesn't
  report them, so count inserts-at-capacity: grep `LruState` and add a `pub(crate)` eviction
  counter OR detect via `len()` before/after; pick the lru-counter approach and unit-test it
  in `lru.rs` — additive, no behavior change).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): keyed limiter — lru bucket
  store, documented eviction (spec §3.2.2)`.

### Task 2.3: bulkhead + load shedding

**Files:** `src/stdlib/resilience.rs`, `src/stdlib/sync.rs` (visibility only).

- [ ] **Step 1 (failing tests, §10 bulkhead rows):** cap honored (limit 2, three concurrent
  `bh.run` of a parking async fn → at most 2 in flight — observe via a counter array the fns
  bump); queue boundary (limit 1, queue 1: first runs, second parks, third gets IMMEDIATE
  `bulkhead-full` — assert the third resolves before the first finishes); all-paths release
  (panicking fn → permit returned → next run succeeds; `recover` around it); waiting-counter
  decrement on panic-while-parked is unreachable (parking can't panic) but deadline expiry
  while parked decrements (covered in Phase 4 — leave a `// deadline test added in Task 4.4`
  marker the Phase-6 review greps as RESOLVED).
- [ ] **Step 2:** make `sync.rs` semaphore internals callable: `pub(crate) fn
  semaphore_state(...)`/expose `sync_acquire`/`sync_release` as `pub(crate)` (no behavior
  change; the existing sync tests stay green untouched). Implement `bulkhead(opts)` (`__sem`
  = a real semaphore handle field) + `run` per §3.3 (shed check + `__waiting` bookkeeping in
  synchronous sections; the `sync_with_permit` all-paths-release shape).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): bulkhead — semaphore cap,
  bounded wait queue, O(1) shed (spec §3.3)`.

### Task 2.4: retry v2 — compatible `task.retry` extension + the policy object

**Files:** `src/stdlib/task_mod.rs`, `src/stdlib/resilience.rs`.

- [ ] **Step 1 (failing tests):** the THREE shipped retry tests still green UNTOUCHED (run
  them first — they are the compat contract); new: `backoff: "fixed"` (counter-observable:
  attempts happen, no timing assertion); `jitter: "none"`/`"full"` accepted (`"full"` bounds
  test at the unit level on the delay-computation helper — extract `compute_retry_delay(
  attempt, base, max, backoff, jitter, rand) -> u64` as a pure fn and unit-test its bounds
  exhaustively, including the `1u64 << shift` cap behavior preserved from v1);
  `retryOn: "error"` (err-pair fn retried; exhaustion returns the LAST pair with
  `code: "retries-exhausted"` folded in iff absent); `retryIf` short-circuit (predicate false
  → one attempt only; predicate panic re-raises); `budget` on `task.retry` → Tier-2 with the
  spec's exact message; budget on the policy: `resilience.retry({attempts: 5, budget: 0.5,
  baseMs: 1})` — drive enough failing calls that `__retriesSpent` hits the ratio → further
  calls behave as exhausted immediately (count-based, no clocks).
- [ ] **Step 2:** refactor `task_retry` into a shared engine `retry_engine(interp, fn, cfg,
  budget_state: Option<&Value /* the policy object */>, span)` in `task_mod.rs`; `task.retry`
  parses opts → cfg (new keys additive, old defaults bit-identical); `resilience.retry(opts)`
  builds the tagged policy (config + `__attemptsSeen`/`__retriesSpent` + `__local`); the
  policy's `call` routes to the same engine with budget state. Jitter keeps `retry_rand_f64`
  and its SP9 exemption comment (extend the comment to note v2 `"full"` mode shares it).
- [ ] **Step 3:** green BOTH configs (including `--no-default-features` WITHOUT `resilience`
  — `task.retry`'s new keys are core and must work there; the policy object is feature-gated).
  Commit — `feat(task,resil): retry v2 — fixed/full-jitter/retryOn/retryIf additive;
  stateful budget policy (spec §3.4)`.

### Task 2.5: fallback

- [ ] **Step 1 (failing tests):** ok value passes through as `[v, nil]`; err pair → `fb(err)`
  called with the err object; panic → `fb` called with `{message}` of the panic (consumed —
  documented); `fb` panic re-raises; `fb` result normalized to a pair; async `fn`/`fb` driven.
- [ ] **Step 2:** implement `resilience.fallback(fn, fb)` per §3.5 (module fn, not a policy
  object — no tag, no hook involvement).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): fallback (spec §3.5)`.

### Task 2.6: Phase 2 independent review

- [ ] Reviewer: full suite + clippy BOTH configs; probes: limiter `refillPerSec: 0` bucket
  drains and never refills; `tryAcquire(0)`/negative n (Tier-2); keyed limiter with 10k keys
  (memory bounded — `maxKeys` honored); bulkhead `queue: 0` (sheds immediately when full —
  valid config); retry `retryOn: "both"` matrix; `task.retry` v1 surface diffed against main
  (`git diff main -- src/stdlib/task_mod.rs` reviewed line-by-line for behavior changes —
  ONLY additive paths allowed); the `compute_retry_delay` unit tests cover attempt ≥ 63
  (shift cap). Confirm no `RefCell`/ObjectCell borrow spans an await in any new code (grep
  `.borrow` near `.await` in the new fns).

---

## Phase 3 — Unit C: singleflight + memoize

### Task 3.1: the per-isolate resilience side-state on `Interp`

**Files:** `src/interp.rs`, `src/stdlib/resilience.rs`.

- [ ] **Step 1:** add `#[cfg(feature = "resilience")] pub(crate) resilience:
  RefCell<crate::stdlib::resilience::ResilState>` to `Interp` (grep how the telemetry state
  field is declared/initialized and mirror). `ResilState { flights: IndexMap<String,
  crate::task::SharedFuture>, registry: ResilRegistry }` (registry filled in Phase 5 —
  declare it now as an empty struct to avoid a second Interp touch).
- [ ] **Step 2:** compile-only commit (both configs, incl. `--no-default-features`) —
  `feat(resil): per-isolate ResilState on Interp (spec §3.6/§6.1)`.

### Task 3.2: singleflight

- [ ] **Step 1 (failing tests, §10 rows):**

```rust
#[tokio::test]
async fn singleflight_collapses_concurrent_calls() {
    let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return 42 }
let f1 = resilience.singleflight("k", fetchIt)
let f2 = resilience.singleflight("k", fetchIt)
print(await f1); print(await f2); print(calls[0])
"#).await;
    assert_eq!(out, "42\n42\n1\n");
}
// + PANIC PROPAGATION: a panicking flight delivers the SAME panic message to BOTH
//   awaiters (two recover()s, both err messages equal — the spec §3.6 SharedFuture
//   argument, pinned);
// + key reusable after settle (sequential: run, await, run again → fn called twice);
// + table emptied after success AND failure (expose a test-only
//   `resilience.__flightCount()` builtin? NO — assert behaviorally: re-fly works and a
//   Rust-level unit test on ResilState via run_source_with_interp checks
//   interp.resilience.borrow().flights.is_empty());
// + non-string key → Tier-2.
```

- [ ] **Step 2:** implement per §3.6 (insert handle, `spawn_local` driver resolving the CELL
  and removing the entry post-resolve in all paths; drive a returned future inside the
  driver; the flights borrow is never held across an await — clone the handle out).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): singleflight — one flight per
  key, panic fan-out, no result caching (spec §3.6)`.

### Task 3.3: memoize

- [ ] **Step 1 (failing tests):** stampede (N concurrent `cache.get("k", fn)` → one `fn`
  run, all get the value); hit/miss counters via `stats()`; TTL boundary under the virtual
  clock (ttlMs 100: hit at +99, miss at +101 — `run_source_deterministic` + the grounded
  sleep-advance mechanics from Task 1.3 Step 2); eviction via `max` (lru semantics); errors
  and panics NOT cached (failing fn → next get retries); `delete`/`clear`/`len`.
- [ ] **Step 2:** implement per §3.7 (`__store` lru handle of `{value, atMs}` entries;
  `__sfPrefix` from a per-isolate monotonically-bumped int in `ResilState` so two caches
  never share flight keys; only success stored).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): memoize — lru + ttl +
  singleflight stampede protection (spec §3.7)`.

### Task 3.4: Phase 3 holistic review (Units A–C combined)

- [ ] Reviewer: full suite + clippy BOTH configs; the kind×method sweep test now exists and
  passes (every `RESIL_KINDS` × method-set pair either works or raises the exact
  `<kind> policy has no method '<name>'` — write it if the implementer didn't); singleflight
  driver leak hunt (spawn flights, drop all awaiters mid-flight, assert the isolate exits
  cleanly and `flights` empties — cancel-on-drop interplay per §3.6 lifecycle); memoize +
  keyed-limiter under `--no-default-features --features resilience` (lru/sync are core —
  prove it); re-run the four-mode hand check on a combined scratch program.

---

## Phase 4 — Unit D: task-locals, deadline propagation, I/O consult sites (THE seam)

### Task 4.1: `TASK_LOCALS` + scope/capture + root scope + the two Gate-14 fixes

**Files:** `src/interp.rs`, `src/vm/run.rs`, `src/lib.rs` (root-scope call sites),
`src/stdlib/telemetry/mod.rs` (doc fix).

- [ ] **Step 1 (failing test — the four-mode inheritance probe):**

```rust
#[tokio::test]
async fn deadline_local_inherited_by_spawned_async_fn() {
    let out = run(r#"
import * as resilience from "std/resilience"
async fn child() { return resilience.deadlineRemaining() != nil }
let [v, err] = resilience.deadline(60000, async fn() {
    let f = child()              // spawned WHILE the deadline local is set
    return await f
})
print(v)
print(resilience.deadlineRemaining())   // restored: nil at top level
"#).await;
    assert_eq!(out, "true\nnil\n");
}
// This test MUST be added to a four-mode-exercised path: also write the .as twin into
// the differential corpus via examples/resilience.as later (Phase 6) — the VM spawn-site
// wrap (vm/run.rs:1747) is exactly what this catches when missing.
```

  (At this point `deadline`/`deadlineRemaining` don't exist — implement the minimal core in
  this task: the locals + the two fns; enforcement racing arrives in Task 4.2.)
- [ ] **Step 2:** `src/interp.rs`: `TaskLocals` + `TASK_LOCALS` + `task_locals_capture()` +
  `task_locals_scope(parent, fut)` + `deadline_remaining_ms(&self)` per spec §5.1/§5.4 —
  CORE (no cfg; the consult sites live in feature-gated modules but the seam is engine
  infrastructure, like `SpanStatus`). Generalize `telemetry_root_scope` →
  `ambient_root_scope` (telemetry scope when that feature is on + the locals scope, always):
  grep every `telemetry_root_scope` call site (`lib.rs:542` + the others) and update.
- [ ] **Step 3:** wrap the FIVE spawn sites (spec §5.1 table): tree-walker
  `interp.rs:5321/5908/5982` (add locals beside the existing telemetry capture);
  VM `vm/run.rs:1747/5709` (add BOTH locals and the missing `telemetry_scope` — **Gate-14 fix
  #1**, with its own regression test: a VM-mode async fn body's span parents correctly —
  grep how telemetry tests assert lineage (`telemetry/mod.rs:756` area) and add the VM-mode
  twin). Fix #2: correct the stale telemetry/mod.rs doc-comment ("not in default" →
  default-on).
- [ ] **Step 4:** implement `resilience.deadline(ms, fn)` (set/restore only — race in 4.2),
  `deadlineRemaining()`, plus the §5.2 nesting-shrinks rule + restore-on-all-exits (panic
  test). Time via `clock_monotonic_ms`.
- [ ] **Step 5:** green BOTH configs. Commit — `feat(resil): TASK_LOCALS seam — copy-on-spawn
  in BOTH engines, root scope, deadline get/set; fix VM telemetry-scope gap (spec §5.1-§5.3,
  §1 fixes)`.

### Task 4.2: deadline enforcement (the race) + policy integration

- [ ] **Step 1 (failing tests):** `deadline(30, sleeps200ms)` → `[nil, {code:
  "deadline-exceeded"}]` (margin 10×: sleep 500ms, deadline 50ms); already-expired nesting
  (`deadline(0, fn)` → immediate, fn never runs — counter proves it); nested shrink
  (outer 60000, inner 120000 → inner's effective remaining ≤ outer's; assert via
  `deadlineRemaining()` comparisons, not absolute values); limiter-acquire and bulkhead-run
  park-with-budget (Task 2.3's marker resolved: bulkhead full + 50ms deadline → caller gets
  `deadline-exceeded` ~50ms, `__waiting` back to 0).
- [ ] **Step 2:** implement the `task_timeout`-shaped select in `deadline` (spec §5.2(a));
  thread `deadline_remaining_ms` into limiter `acquire` and bulkhead `run` parking (§3.2.1/
  §3.3 — race the park against the budget).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): deadline enforcement race +
  budget-aware limiter/bulkhead parking (spec §5.2/§5.4)`.

### Task 4.3: trace id + log/telemetry attachment

- [ ] **Step 1 (failing tests):** `withTrace("t-1", fn)` → `traceId()` inside == `"t-1"`,
  restored after; inherited into spawned async fns (the 4.1 probe shape); a `log.info` inside
  a trace scope carries `traceId` in its serialized record (grep the log tests for how
  records are captured/asserted and mirror); outside any scope → no field (zero-cost None).
- [ ] **Step 2:** implement `withTrace`/`traceId`; one `try_with` in the log record builder
  (`#[cfg(feature = "log")]` site) and the SP12 span-attr attach (`#[cfg(feature =
  "telemetry")]`, in `telemetry_open_span`'s attr assembly — grep first).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): trace-id local + log/telemetry
  attachment (spec §5.5)`.

### Task 4.4: deadline-aware I/O consult sites

**Files:** `src/stdlib/net_http.rs`, `src/stdlib/postgres.rs`, `src/stdlib/redis.rs`,
`src/stdlib/sqlite.rs`.

- [ ] **Step 1 (failing tests):** http — expired deadline → immediate `[nil, {code:
  "deadline-exceeded"}]` with NO connection attempt (point at a non-routable address; assert
  fast return, e.g. elapsed < 1s vs the connect timeout); http clamp — a request with
  `timeout: {total: 60000}` under a 50ms deadline against a never-responding local listener
  (bind a TcpListener in the test, never accept) → deadline err in ~50ms. postgres/redis —
  pre-check tests (expired → immediate err, no op issued); budget-wrap tests require a live
  server → write them as the repo's existing postgres/redis tests are written (grep for how
  those suites gate on an available server — env-var-gated integration tests; mirror that
  gating, never a silently-skipped assert). sqlite — pre-check only (expired → err before
  the query; an in-flight sync query is NOT interrupted — document, don't test the
  impossible).
- [ ] **Step 2:** implement per spec §5.4 (each site inside its module's existing feature
  cfg; the http clamp at the `net_http.rs:572-592` builder; postgres/redis wrap via the
  `task_timeout` select shape around the op await — VERIFY per driver what a mid-op abandon
  does to the connection (read the driver code/docs) and write the docs sentence from
  evidence).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): deadline consult sites —
  http clamp, pg/redis budget-wrap, sqlite pre-check (spec §5.4)`.

### Task 4.5: the zero-cost gate (Gate 12) — REQUIRED BENCH

**Files:** `tests/vm_bench.rs` (or its harness — read its module doc for the documented
invocation), `bench/RESILIENCE_RESULTS.md` (started).

- [ ] **Step 1:** add a `resil_zero_cost_gate` to the vm_bench harness modeled EXACTLY on
  `dbg_zero_cost_gate` (grep it; same-session A/B, geomean assertion): an async-spawn-heavy
  workload (the `async_inline` shape — 100k+ trivial async calls) and a call+member-heavy
  workload, measured with locals NEVER set. The A/B here is current-branch vs the assertion
  threshold the dbg gate uses (≈1.0×, with its tolerance) — and additionally run the
  same-session main-vs-branch comparison by hand and record it in
  `bench/RESILIENCE_RESULTS.md` (Gate 16: both numbers in one session, one machine).
- [ ] **Step 2:** Gate-17: run the full `vm_bench` spec/tw geomean — the ≥2× floor holds
  (RESIL touched the async spawn sites and the method-dispatch ladder — this is the proof).
  Record in the report. If EITHER gate fails: the seam's home is wrong — fix (e.g. hoist the
  `try_with` behind the existing telemetry capture's cfg structure, or cache the capture),
  never relax.
- [ ] **Step 3:** RSS row (Gate 18): `/usr/bin/time -l` on the bench corpus, main vs branch,
  recorded. Commit — `bench(resil): zero-cost task-local gate + geomean floor + RSS (Gates
  12/16/17/18)`.

### Task 4.6: Phase 4 independent review

- [ ] Reviewer: full suite + clippy BOTH configs; runs the zero-cost gate THEMSELVES and
  sanity-checks the methodology (same-session, release build); probes: deadline inside a
  generator body (resume-time semantics — write the probe from spec §5.1's NOT-wrapped table
  and assert the documented behavior); deadline does NOT cross a `worker fn` boundary (probe
  + assert empty locals inside); http handler tasks start with fresh locals (a serve-level
  deadline set before `listen` is NOT visible in a handler); locals restored after a
  panicking `deadline` body (recover + `deadlineRemaining() == nil`); `git diff` of the five
  spawn sites reviewed for borrow-across-await; the VM telemetry fix has its regression test.

---

## Phase 5 — Unit E: registry, `/metrics`, health, the HTTP wrapper

### Task 5.1: the metrics registry + policy instrumentation

**Files:** `src/stdlib/resilience.rs` (ResilRegistry), policy engines (counter bumps),
`src/interp.rs` only if the telemetry mirror needs a helper.

- [ ] **Step 1 (failing tests):** after a breaker trip: registry contains
  `breaker_state{name="t"} == 1.0` and `breaker_calls_total{name="t",result="failure"} == 2`
  (assert via a Rust-level unit test over `interp.resilience.borrow().registry` using
  `run_source_with_interp`, `lib.rs:707`); limiter/bulkhead/retry/memoize/singleflight/
  deadline counters per the §6.1 metric set (one assertion each, all in one scenario run);
  telemetry mirror: with telemetry initialized in capture mode (grep
  `Interp::telemetry_capture` for the test seam), a breaker call records a counter point
  through the soft hook; with telemetry NOT initialized, nothing breaks.
- [ ] **Step 2:** implement `ResilRegistry` (counters+gauges, `IndexMap<String,
  IndexMap<String, f64>>` name → label-key → value, label-key via the `attr_key`
  canonicalization — reuse/lift `telemetry/model.rs:388`); bump sites inside the policy
  engines (synchronous, behind the already-held state borrows where possible); the
  `#[cfg(feature = "telemetry")]` mirror via `telemetry_register_instrument`/
  `telemetry_record_metric` (instrument ids cached per policy in `__metricIds`? — NO: keep a
  name-keyed instrument-id map in `ResilState`, one registration per metric name) and the
  `#[cfg(feature = "log")]` transition `log.debug`.
- [ ] **Step 3:** green BOTH configs + `--features resilience` without telemetry/log
  (cfg matrix builds). Commit — `feat(resil): per-isolate metrics registry + policy
  instrumentation + telemetry/log mirrors (spec §6.1)`.

### Task 5.2: `metricsHandler()` — Prometheus text

- [ ] **Step 1 (failing test):** end-to-end through a REAL server: `server.bind` port 0 →
  mount `resilience.metricsHandler()` at `/metrics` → trip a breaker → `serve({maxRequests:
  1})` + an http client GET (the established http_server test choreography — grep its tests
  and mirror) → body contains `# TYPE ascript_resilience_breaker_state gauge` and
  `ascript_resilience_breaker_state{name="t"} 1`; content-type `text/plain; version=0.0.4`;
  label escaping test (a policy named `a"b\n` renders escaped — construct via the registry
  unit, not the server).
- [ ] **Step 2:** implement per §6.2: the `NativeKind::Resilience` NativeMethod handler
  (returned value `Value::NativeMethod` whose `method` is `"__metrics"`); wire the
  `call_native_method` dispatch arm for `NativeKind::Resilience` (grep how `call_lru_method`
  is routed from `call_native_method` and mirror); deterministic rendering (insertion order +
  sorted labels).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): Prometheus text /metrics
  handler (spec §6.2)`.

### Task 5.3: `health({checks})` + `handler(policies, fn)`

- [ ] **Step 1 (failing tests):** health — all-pass → 200 with `{"status":"ok"}` JSON +
  per-check detail; one failing/panicking/timing-out check → 503 with that check's
  `{ok: false, error}` and the OTHERS still reported (containment); empty `{}` → 200
  (liveness). handler — full status mapping through a real server: rate-limited → 429 (+
  `retry-after` header present), bulkhead-full → 503, breaker-open → 503, `deadlineMs`
  expiry → 504, plain handler error-pair → the server's existing 500 path UNCHANGED; key
  extractor fn receives the request object; every policy key optional (a `{}` wrapper is the
  identity).
- [ ] **Step 2:** implement per §6.3/§6.4 (same NativeMethod mechanism; checks/policies/fn
  in the NativeObject `fields`; the §6.4 fixed order limiter→bulkhead→breaker→deadline→fn;
  per-check timeout reuses `deadline`).
- [ ] **Step 3:** green BOTH configs. Commit — `feat(resil): health/readiness handlers +
  resilience.handler HTTP wrapper (spec §6.3/§6.4)`.

### Task 5.4: Phase 5 independent review

- [ ] Reviewer: full suite + clippy BOTH configs; probes: `/metrics` under concurrent
  traffic (no borrow panic — hammer with `maxConcurrent` default while tripping policies);
  Prometheus output validated against the exposition format (run it through `promtool check
  metrics` if available locally; otherwise validate the grammar by eye against the format
  spec — escaping, TYPE lines, no duplicate series); a health check that returns a pair
  `[true, nil]` vs bare `true` (both pass per §6.3); `resilience.handler` with ONLY
  `deadlineMs`; wrapper composes with `server.use` middleware (trace middleware + wrapped
  route in one server — the gateway shape); confirm `required_cap("resilience", *) == None`
  is right by probing under `--sandbox` (metrics/health still serve — the SERVER needs net,
  the handlers don't).

---

## Phase 6 — Unit F: corpus, negative space, docs, meta, FINAL gates

### Task 6.1: examples (Gate 9) + four-mode differential

**Files:** `examples/resilience.as`, `examples/advanced/resilient_gateway.as`.

- [ ] **Step 1:** `examples/resilience.as` — intro, deterministic output (~60 lines):
  breaker trip + rejection code + `reset` (count-based — fully deterministic); limiter
  `tryAcquire` exhaustion (`refillPerSec: 0.001` so no refill races CI); keyed limiter
  per-key isolation; bulkhead shed; `task.retry` with `retryOn: "error"` + `retryIf`;
  fallback; singleflight collapse (print call counter); memoize stampede; `deadline` over a
  long sleep (10× margin) + nested-shrink print; edge cases: expired deadline fast-fail,
  validation `recover`s. NO wall-clock-dependent values printed (only verdicts/counters).
- [ ] **Step 2:** `examples/advanced/resilient_gateway.as` — production-shaped per spec §10:
  trace middleware (`withTrace` from header), `resilience.handler`-wrapped route (keyed
  limiter + bulkhead + breaker + `deadlineMs`), an explicitly-composed
  `deadline → breaker → retry` backend call with the §3.5 wrap-order comments, `/metrics` +
  `/readyz` mounted, the §7.2 `worker class GlobalLimiter` actor, a self-driving client
  section + `serve({maxRequests: N})` so it RUNS TO COMPLETION deterministically (the
  http_server test choreography at example scale — if a bound-port example cannot be made
  byte-deterministic across modes, split the server part behind the `EXAMPLE_SKIPS` pattern
  like `server_multicore.as` and keep the policy composition part in-corpus; grep
  `EXAMPLE_SKIPS` and follow SRV's precedent — decide by EVIDENCE, run it 10× per mode).
- [ ] **Step 3:** four-mode by hand: `run`, `--tree-walker`, generic-VM, `build`→`.aso` —
  byte-identical. `cargo test --test vm_differential` BOTH configs (confirm the new examples
  are picked up — grep the test output). `ascript fmt` both (idempotent); `ascript check`
  (zero diagnostics — Gate 5 on the new files).
- [ ] **Step 4:** Commit — `feat(resil): example corpus — intro + resilient gateway,
  four-mode verified (Gate 9)`.

### Task 6.2: negative space + Gate-5 sweep + arity table

- [ ] **Step 1:** extend `tests/resil_negative_space.rs`: no new opcode (pin the opcode
  count the way `srv_negative_space.rs`/`par` pins do — read it, reuse the technique);
  hook-order pin (a value that is BOTH `__resil`-tagged and `__kind`-tagged routes to schema
  — constructs the §2.3 ordering note as a test… ground first: such a value has `__kind` ∈
  SCHEMA_KINDS → schema wins by branch order; assert that); `OptMember` non-routing pin;
  the retry-v1 integration pins from Phase 0 still green.
- [ ] **Step 2:** `src/check/std_arity.rs`: check whether `std/schema`/`std/task` are
  curated; add `resilience.*` min-arities iff the pattern fits (constructors 1, fallback 2,
  singleflight 2, deadline 2, withTrace 2; `max=None` per the table's rule). Run
  `cargo test --test check` BOTH configs — Gate 5 zero `type-*` on `examples/**` including
  the new files.
- [ ] **Step 3:** Commit — `test(resil): negative-space pins + arity entries + Gate-5 sweep`.

### Task 6.3: docs (Gate 13)

**Files:** `docs/content/stdlib/resilience.md` (new), `docs/assets/app.js`,
`docs/content/language/workers.md`, `docs/content/stdlib/async.md`, `README.md`.

- [ ] **Step 1:** `resilience.md` — full reference: every constructor/option table with
  defaults; the §2.4 error-code table verbatim; the call-position-only note (bare reads read
  fields); the §3.5 wrap-order section with BOTH timeout-inside/outside-retry diagrams and
  the breaker-vs-retry ordering guidance; deadline semantics (nesting shrinks; what's
  preemptible and what isn't — sqlite honesty); §3.2.2 eviction semantics + the
  `sync.rateLimiter` relationship; the **per-isolate honesty section** (§7.1 verbatim tone:
  N workers = N breakers, the Envoy analogy, `__local` loud refusal, the actor pattern with
  the §7.2 code); metrics reference (the full §6.1 metric set table + the per-isolate scrape
  caveat); health/handler reference.
- [ ] **Step 2:** **add `"stdlib/resilience"` to `NAV` in `docs/assets/app.js`** (the orphan
  gotcha — sidebar AND cmd-K derive from it); cross-link from `workers.md` (per-isolate
  state + actor pattern) and `async.md` (task.retry v2 keys documented where retry lives;
  link over). `README.md` stdlib table row. Serve the site (`cd docs && python3 -m
  http.server`) and click through (in-content links are page-relative).
- [ ] **Step 3:** Commit — `docs(resil): stdlib/resilience reference + NAV + workers/async
  cross-links (Gate 13)`.

### Task 6.4: meta-docs + status flips

**Files:** `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`.

- [ ] **Step 1:** `CLAUDE.md`: a terse RESIL note in the house style (tagged-Object policies
  + the hook ladder position; `TASK_LOCALS` copy-on-spawn seam + the five spawn sites +
  zero-cost gate; det-routed verdicts vs timing-only sleeps; per-isolate honesty + `__local`
  marker; deadline consult sites; no ASO change). Mention the VM telemetry-scope fix so the
  next reader doesn't re-derive the asymmetry.
- [ ] **Step 2:** `roadmap.md` milestone entry (what shipped, review findings, the two
  Gate-14 fixes); `goal-perf.md` — flip RESIL to ✅ with a one-line result + the zero-cost
  number; correct any spec-vs-shipped drift in the RESIL stanza (e.g. brief-vs-code deltas,
  spec §11).
- [ ] **Step 3:** `grep -rn "resilience" docs/ README.md CLAUDE.md` — every mention
  consistent with shipped behavior. Commit — `docs(resil): CLAUDE.md + roadmap + goal-perf
  status (RESIL done)`.

### Task 6.5: FINAL holistic review + full gates checklist

The independent holistic reviewer runs EVERYTHING and ticks each box with evidence (paste
command output summaries into the review note):

- [ ] `cargo build` + `cargo build --no-default-features` clean; plus the cfg matrix:
  `--no-default-features --features resilience`.
- [ ] `cargo clippy --all-targets` AND `--no-default-features --all-targets` — zero warnings
  (Gate 2).
- [ ] `cargo test` AND `cargo test --no-default-features` — full suites green (Gate 3).
- [ ] `cargo test --test vm_differential` BOTH configs — new examples in the corpus,
  tree-walker == specialized == generic; `.aso` mode covered (Gate 1).
- [ ] Examples four-mode byte-identical (re-run by hand), fmt-idempotent, check-clean
  (Gates 9/11); Gate 5 zero `type-*` on `examples/**` BOTH configs.
- [ ] `tests/resil_negative_space.rs` green (ASO untouched, no opcode, hook order, retry-v1
  pins). Gate-15 posture confirmed: no new engine config → no new differential mode; fuzz
  targets unchanged and smoked (`worker_serialize` run per the repo's documented invocation).
- [ ] Gate 12/16/17/18: the `resil_zero_cost_gate` passes in release; the spec/tw geomean
  ≥2× floor re-run green; `bench/RESILIENCE_RESULTS.md` has same-session A/B + RSS.
- [ ] Determinism: the seeded/virtual-clock tests green; a Record/Replay round-trip of a
  breaker-cooldown scenario replays byte-identically (write the probe if missing — the §8
  claim must have a test).
- [ ] No `RefCell`/ObjectCell borrow across `.await` in ANY new/modified code (reviewer
  greps `.borrow` within new fns and reads each await neighborhood); no
  `unwrap()`/`expect()`/`panic!` reachable from malformed input on new paths (Gate 14).
- [ ] Adversarial pass (Gate 14): re-probe spec §2.4's code table and §10's test-matrix rows
  against the release binary row by row — messages and codes verbatim; hunt: policy objects
  mutated by hand (`b.__state = "open"` — document-only, must not crash the engine); a
  policy crossing `run_in_worker` (field-path panic naming `__local`); `/metrics` while a
  breaker transitions concurrently; deep `deadline` nesting (1000 levels — Rc chain, no
  stack issue); `withTrace` inside `deadline` inside `withTrace` (restore ordering).
- [ ] Both Gate-14 pre-existing fixes (VM telemetry scope; telemetry doc-comment) landed
  with their regression tests.
- [ ] `CLAUDE.md`/`roadmap.md`/`goal-perf.md` accurate.
- [ ] Merge: `git checkout main && git merge --no-ff feat/resilience-stdlib` only after every
  box above is ticked.
