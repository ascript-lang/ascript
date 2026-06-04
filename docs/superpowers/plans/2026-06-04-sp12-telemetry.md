# SP12 — `std/telemetry` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `std/telemetry` — spans + metrics + analytics events with three hand-rolled
exporters (OTLP HTTP/JSON, Sentry, PostHog) behind one opt-in `telemetry` Cargo feature — plus the
soft, runtime-installed `Interp` hook that SP11 (`std/ai`) consumes. Telemetry is a runtime no-op
until `telemetry.init(...)`; both modules build independently.

**Architecture:** Six phases. **F0** scaffolds the feature + a no-op module + the `Interp` state &
soft hook. **F1** tracing (spans + scoped helper + parenting). **F2** OTLP exporter (the span/metric/
log wire shaping). **F3** Sentry exporter. **F4** PostHog exporter + `capture`/`identify`. **F5** the
SP11-facing soft-hook surface finalization + docs + holistic review. Each phase is TDD, ends green on
both feature configs + clippy + the capture-mode unit tests, and gets an independent review.

**Tech Stack:** Rust. Stdlib module over `Value`; `Interp`-stateful singleton (mirrors `std/log`).
Hand-rolled exporters over the pooled `reqwest::Client` in `src/stdlib/net_http.rs`. JSON via
`stdlib::json::to_json_lossy`. `!Send` current-thread runtime + `LocalSet`; never hold a `RefCell`/
`resources` borrow across `.await` (take-out-across-await).

**Spec:** `docs/superpowers/specs/2026-06-04-sp12-telemetry-design.md`.

**Branch:** `feat/sp1-engine-parity` (per task context). Ships before SP11.

---

## Conventions for every task

- **Capture-mode test harness:** unit tests run `.as` source through `ascript::run_source` with the
  test `Interp` (capture sink), then read the recorded exporter HTTP bodies via the Rust accessor
  `interp.telemetry_capture()` and assert against expected JSON. **No socket, no secret** in any
  default test. Model the harness on `src/stdlib/log.rs`'s capture pattern (read `log_output()` and
  the `call_log` tests in `src/interp.rs` first).
- **Read-before-write:** before editing, read the neighboring module that already does the thing —
  `src/stdlib/log.rs` (stateful singleton, `exports()`), `src/stdlib/net_http.rs` (reqwest client,
  SSE/native-handle + take-out-across-await), `src/interp.rs` (`call_log`, `OutputSink`,
  `ResourceState`, `take_resource`/`return_resource`).
- **Gate after each phase (paste tails):**
  `cargo test --features telemetry --test telemetry 2>&1 | tail`;
  `cargo test 2>&1 | tail` (0 failures, telemetry OFF in default);
  `cargo test --features telemetry 2>&1 | tail` (0 failures);
  `cargo build --no-default-features 2>&1 | tail` (builds, module cfg'd out);
  `cargo clippy --all-targets 2>&1 | tail` AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean);
  `cargo clippy --features telemetry --all-targets 2>&1 | tail` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`).
- **No `opentelemetry`/`sentry`/`posthog` crate may be added** (spec §6). If a task seems to need one,
  stop and re-read §6 — the wire shapes are hand-rolled JSON.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## Phase F0 — Feature scaffold + no-op module + Interp state & soft hook

**Files:** `Cargo.toml` (feature); `src/stdlib/telemetry/mod.rs` (new); `src/stdlib/mod.rs`
(register both arms); `src/interp.rs` (`telemetry` state cell, `call_telemetry`, the `telemetry_*`
hook methods + `SpanStatus`, capture accessor).

### Task F0.1: Cargo feature + empty module that builds in all configs

- [ ] **Step 1 — Add feature.** In `Cargo.toml [features]` add `telemetry = ["data", "net"]`. Do
  **NOT** add it to `default`. No new `[dependencies]` entry.
- [ ] **Step 2 — Create `src/stdlib/telemetry/mod.rs`** with `pub fn exports() -> Vec<(&'static str,
  Value)>` returning the builtin bindings (`init`, `otlp`, `sentry`, `posthog`, `flush`, `shutdown`,
  `startSpan`, `span`, `counter`, `histogram`, `gauge`, `capture`, `identify`) each as
  `super::bi("telemetry.<name>")`, and a `pub async fn call(...)` stub that, for now, returns a
  Tier-2 "not yet implemented" panic for every func except the no-op set (see F0.3). Match the shape
  of `src/stdlib/log.rs::exports`.
- [ ] **Step 3 — Register** in `src/stdlib/mod.rs`: `#[cfg(feature = "telemetry")] pub mod
  telemetry;`, and add `#[cfg(feature = "telemetry")] "std/telemetry" => telemetry::exports(),` to
  `std_module_exports` and the matching arm to the `call` router.
- [ ] **Step 4 — Run:** `cargo build --features telemetry`, `cargo build`, `cargo build
  --no-default-features` all compile. Commit: `feat(telemetry): scaffold std/telemetry feature + module skeleton`.

### Task F0.2: Interp telemetry state + soft hook (cfg-bridged, always-present signatures)

- [ ] **Step 5 — Read** `src/interp.rs` around `log_level`/`log_format`/`OutputSink`/`call_log`/
  `log_output` and the `ResourceState` enum + `take_resource`/`return_resource`.
- [ ] **Step 6 — Add state.** On `Interp`: a `telemetry: RefCell<Option<TelemetryState>>` (None =
  uninitialized) and a capture sink (a `RefCell<Vec<CapturedRequest>>` populated in capture mode).
  Define `SpanStatus { Unset, Ok, Error }` in a **core (non-feature-gated) module** so SP11 can name
  it with telemetry off. `TelemetryState` itself is `#[cfg(feature = "telemetry")]`.
- [ ] **Step 7 — Add the soft hook**, methods on `Interp` with **always-present signatures** (the
  body cfg-bridged): `telemetry_active`, `telemetry_span_start`, `telemetry_span_set`,
  `telemetry_span_event`, `telemetry_span_end` (signatures per spec §3). With `telemetry` OFF the
  bodies are inert (`false` / `None` / no-op); with it ON they delegate to `TelemetryState`. Add
  `pub fn telemetry_capture(&self) -> Vec<CapturedRequest>` for tests.
- [ ] **Step 8 — Run:** `cargo build` (telemetry off — the hook methods exist & are inert) and
  `cargo build --features telemetry`. **Critical check:** confirm `telemetry_active` etc. are
  callable with the feature OFF (this is what makes SP11 build independently). Commit:
  `feat(interp): telemetry state cell + always-present soft hook (inert when feature off)`.

### Task F0.3: no-op-when-uninitialized behavior + first tests

- [ ] **Step 9 — Failing test** in `tests/telemetry.rs` (run with `--features telemetry`): a program
  that calls `telemetry.startSpan("x").end()` and `telemetry.counter("c").add(1)` **without** `init`
  produces **no captured requests** and does not error (`run_source` succeeds, `telemetry_capture()`
  empty). Write it; it fails (call stubs panic).
- [ ] **Step 10 — Implement** the no-op path: when `telemetry` is `None`, every call returns an inert
  handle (a `Value::Native` whose methods are no-ops) and enqueues nothing. `init`/`otlp`/`sentry`/
  `posthog` are implemented enough to build descriptors and set `Some(TelemetryState)` (exporters
  still stubbed — they just record into the capture sink in capture mode; real HTTP comes in F2–F4).
- [ ] **Step 11 — Run** the test → green. Phase-F0 gate. Commit:
  `feat(telemetry): no-op-until-init + inert handles + capture sink`.

---

## Phase F1 — Tracing: spans, scoped helper, parenting

**Files:** `src/stdlib/telemetry/mod.rs` (span funcs), `src/interp.rs`
(`ResourceState::TelemetrySpan` + accessors, current-span thread-local, `call_telemetry` span arms).

### Task F1.1: span handle + lifecycle

- [ ] **Step 1 — Failing tests** (`tests/telemetry.rs`, telemetry initialized with a single capturing
  exporter): a `startSpan("s", {attributes:{a:1}})` → `setAttribute`/`addEvent`/`setStatus("ok")` →
  `end()` produces exactly one captured span with the right name/attrs/event/status. A method call
  **after** `end()` is a no-op (no second span, no error).
- [ ] **Step 2 — Implement** `ResourceState::TelemetrySpan` (span id, trace id, parent id, start/end
  ns, attrs, events, status) + accessors; `startSpan`/`setAttribute`/`addEvent`/`setStatus`/`end`
  routed in `call_telemetry`. On `end()`, enqueue the finished span into the pipeline (capture sink
  in tests). Use take-out-across-await if any async work is involved (span ops are sync; flushing is
  async — keep flush out of `end`).
- [ ] **Step 3 — Run** → green. Commit: `feat(telemetry): span handle + lifecycle`.

### Task F1.2: scoped `telemetry.span(name, fn)` + parenting

- [ ] **Step 4 — Failing tests:** (a) `let [v,e] = await telemetry.span("op", async fn(){ return 42
  })` → `v==42`, `e==nil`, one captured span status `ok`, duration ≥0. (b) a callback that panics →
  `[nil, err]`, span status `error` with the message recorded, program continues. (c) **parenting:** a
  `startSpan` inside the callback parents to the scoped span (assert child's `parentId == outer
  spanId`, same `traceId`). (d) **concurrency isolation:** two `telemetry.span` calls driven on
  concurrent `spawn_local` tasks do NOT cross-parent (each child parents to ITS task's span) — this
  guards the spec §9.3 thread-local-across-await correctness point.
- [ ] **Step 5 — Implement** the current-span thread-local stack (model on `coro.rs`'s generator
  stack); push around the callback, save/restore across the `await` so concurrent tasks don't leak
  parents; catch a panic like `recover`, set error status, return `[nil, err]`. Root spans mint a new
  trace id.
- [ ] **Step 6 — Run** → green, especially the concurrency-isolation case. Phase-F1 gate. Commit:
  `feat(telemetry): scoped span helper + current-span parenting (await-safe)`.

---

## Phase F2 — OTLP exporter (HTTP/JSON) for spans, metrics, logs

**Files:** `src/stdlib/telemetry/otlp.rs` (new — wire shaping), `src/stdlib/telemetry/mod.rs`
(`otlp` descriptor, metric instruments, flush), `src/interp.rs`
(`ResourceState::Telemetry{Counter,Histogram,Gauge}`, the send seam + flush task).

### Task F2.1: OTLP span JSON shaping (capture-mode assertion)

- [ ] **Step 1 — Failing test:** init with `telemetry.otlp({endpoint:"http://localhost:4318"})`,
  produce one span, `await telemetry.flush()`, assert the captured request is a POST to
  `…/v1/traces` whose JSON body matches the OTLP `ResourceSpans` shape: `resource.attributes`
  includes `service.name`; the span has hex `traceId`/`spanId`, `startTimeUnixNano`/
  `endTimeUnixNano`, `attributes` as OTLP KeyValue, `status`. (Pin exact keys against spec §2/§3 and
  the OTLP `http/json` proto3-JSON mapping — hex ids, not base64.)
- [ ] **Step 2 — Implement** `otlp.rs`: `serialize_spans(resource, spans) -> Value/JSON string`
  matching the OTLP JSON, and the `otlp` descriptor parsing (endpoint/protocol/headers, env
  fallbacks, `http/protobuf`/`grpc` → Tier-2 panic). Wire the send seam: in capture mode record the
  `{url, headers, body}`; in live mode POST via the pooled reqwest client (take-out-across-await).
- [ ] **Step 3 — Run** → green. Commit: `feat(telemetry): OTLP HTTP/JSON span export`.

### Task F2.2: metrics (counter/histogram/gauge) → OTLP metrics

- [ ] **Step 4 — Failing tests:** counter `.add(1,{route:"/x"})` twice → cumulative `Sum` 2 for that
  attribute set; histogram `.record` → `Histogram` data point; gauge `.set(7)` → `Gauge` 7; flush
  POSTs to `…/v1/metrics` with the OTLP metric JSON.
- [ ] **Step 5 — Implement** the instrument `ResourceState`s + in-process aggregation (cumulative,
  per spec §9.2) + `serialize_metrics`. Idempotent instrument registration (same name → same handle).
- [ ] **Step 6 — Run** → green. Phase-F2 gate. Commit: `feat(telemetry): OTLP metrics (sum/histogram/gauge)`.

---

## Phase F3 — Sentry exporter

**Files:** `src/stdlib/telemetry/sentry.rs` (new), `mod.rs` (`sentry` descriptor + DSN parse), flush.

### Task F3.1: DSN parse + transaction + error envelopes

- [ ] **Step 1 — Failing tests:** (a) `telemetry.sentry({dsn:"https://KEY@o123.ingest.sentry.io/456"})`
  parses to the envelope URL `https://o123.ingest.sentry.io/api/456/envelope/` + the auth header;
  a malformed DSN at `init` → Tier-1 `[nil, err]`. (b) a root span subtree → one Sentry
  **transaction** envelope (newline-delimited envelope: header line + item header + item payload).
  (c) an `error`-status span → an additional Sentry **error event** item.
- [ ] **Step 2 — Implement** `sentry.rs`: DSN parse, envelope construction (transaction from a span
  tree; error event from an error-status span/recovered panic), routed through the send seam.
- [ ] **Step 3 — Run** → green. Phase-F3 gate. Commit: `feat(telemetry): Sentry envelope export (transactions + errors)`.

---

## Phase F4 — PostHog exporter + capture/identify

**Files:** `src/stdlib/telemetry/posthog.rs` (new), `mod.rs` (`posthog` descriptor, `capture`/
`identify`, OTLP-mirror flag).

### Task F4.1: capture/identify → PostHog HTTP

- [ ] **Step 1 — Failing tests:** init with `telemetry.posthog({apiKey:"phc_x"})`;
  `telemetry.capture("signup_completed",{distinctId:"u1",properties:{plan:"pro"}})` + flush → POST
  to `…/capture/` (or `/batch/`) with `api_key`, `event`, `distinct_id`, `properties`;
  `telemetry.identify("u1",{email:"a@b.com"})` → a `$identify` event with `$set` props. With no
  PostHog exporter configured and `mirrorEventsToOtlp` off, `capture` is a no-op.
- [ ] **Step 2 — Implement** `posthog.rs`: capture/identify payloads, batching, send seam; the
  `init({mirrorEventsToOtlp:true})` path additionally emits each capture as an OTLP log record.
- [ ] **Step 3 — Run** → green. Phase-F4 gate. Commit: `feat(telemetry): PostHog capture/identify export`.

---

## Phase F5 — SP11-facing hook finalization, error model, docs, holistic review

**Files:** `src/interp.rs` (finalize hook + flush-on-exit), `tests/telemetry.rs` (error-model +
env-gated live), `docs/content/stdlib/telemetry.md`, README, `examples/`.

### Task F5.1: error model + flush-on-exit + re-init/shutdown

- [ ] **Step 1 — Failing tests:** a flush HTTP failure (capture seam configured to error) does NOT
  panic the program (single stderr warning, dropped); `init` with a missing required config → Tier-1
  `[nil, err]`; `setStatus("bogus")` / `init(42)` / unknown exporter kind → Tier-2 panic; re-`init`
  replaces the pipeline (old flushed); `shutdown()` → subsequent calls are no-ops again.
- [ ] **Step 2 — Implement** the error model (spec §5) + flush-on-process-exit (hook into the same
  exit/shutdown path the binary uses) + re-init/shutdown semantics.
- [ ] **Step 3 — Run** → green. Commit: `feat(telemetry): error model + flush-on-exit + re-init/shutdown`.

### Task F5.2: finalize the SP11 soft hook + a hook-level test

- [ ] **Step 4 — Test** the soft hook directly (Rust): with telemetry initialized,
  `interp.telemetry_span_start("chat openai:gpt-4.1", attrs)` → `…_set` → `…_end(Ok)` produces a
  captured OTLP span carrying the attrs; with telemetry uninitialized, `telemetry_active()==false`
  and `telemetry_span_start` returns `None`. (This is the exact surface SP11 will call.)
- [ ] **Step 5 — Confirm independence:** `cargo build --no-default-features` and `cargo build`
  (telemetry off) — the hook methods compile & are inert. Document the hook contract inline.
- [ ] **Step 6 — Commit:** `test(telemetry): soft-hook surface verified (the SP11 contract)`.

### Task F5.3: docs + example + holistic gate

- [ ] **Step 7 — Docs:** write `docs/content/stdlib/telemetry.md` (init/exporters, spans, metrics,
  events, error model, the SP11 relationship) per the repo's generated-from-source convention; add a
  row to the README stdlib table + nav. Add `examples/advanced/telemetry.as` (deterministic, uses an
  OTLP exporter pointed at a no-op/local endpoint, ends `print("telemetry ok")`) and verify with
  `target/release/ascript --features? run` (build with `--features telemetry`). **Verify every
  documented snippet against the binary.**
- [ ] **Step 8 — Full gate set** (all configs incl. `--features telemetry`, clippy all three configs).
- [ ] **Step 9 — Independent review:** re-read the spec; adversarial hunt over: current-span
  parenting across concurrent `spawn_local` tasks, take-out-across-await in every exporter flush,
  no-op-when-off invariant, the SP11 hook building with telemetry absent, OTLP hex-id correctness.
  Fix any issue at the root.
- [ ] **Step 10 — Final commit** if review surfaced fixes.

---

## Self-review (author)

**Spec coverage:** §1 feature/module → F0; §2 init/exporters → F0.3 + F2–F4; §3 tracing + soft hook
→ F1 + F5.2; §4 metrics/events → F2.2 + F4; §5 error model → F5.1; §6 hand-rolled-no-crate → enforced
by the "no opentelemetry/sentry/posthog crate" convention; §7 testing → capture-mode harness +
env-gated live across all phases. All covered.

**`!Send` discipline:** every exporter flush is async over the pooled reqwest client via take-out-
across-await; no `RefCell`/`resources` borrow held across `.await`; no new Send-assuming crate
(spec §6). The current-span thread-local is saved/restored across `await` (F1.2 step 4d test).

**Independence:** the `telemetry_*` hook methods on `Interp` have always-present signatures with
cfg-bridged bodies (F0.2 step 8 verifies they compile & are inert with the feature off), so SP11
builds with telemetry absent and SP12 builds with ai absent.

**No placeholders:** every task has concrete `.as`/Rust assertions and exact paths/commands. The one
deferred-to-implementer detail is exact OTLP/Sentry/PostHog JSON key spelling — pinned by the cited
specs in the design doc; the implementer validates against recorded fixtures + the env-gated live
collector. `cargo build` (not `cargo test`) is used for `--no-default-features` since the module is
cfg'd out there.
