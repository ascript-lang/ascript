# SP12 — `std/telemetry` (tracing / metrics / events + exporters) — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover stdlib-expansion program. **Pairs with** SP11 (`std/ai`) —
> SP11 emits OpenTelemetry GenAI spans **through** the soft hook this sub-project installs, so
> **SP12 ships first**. See `docs/superpowers/specs/2026-06-04-sp11-ai-design.md`.
> **Derived from** the research proposal `docs/superpowers/specs/2026-06-04-sp11-sp12-ai-telemetry-PROPOSAL.md`
> (API shapes, citations) — this design resolves the proposal's open forks per owner decisions.

**Goal:** Add a thin, vendor-neutral observability facade `std/telemetry` to the AScript standard
library: tracing (spans), metrics (counter/histogram/gauge), and analytics events
(`capture`/`identify`), delivered through **three exporters in v1 — OTLP (HTTP/JSON) + Sentry +
PostHog** behind one `telemetry` Cargo feature. Telemetry is **opt-in at runtime** (a no-op until
`telemetry.init(...)` runs) and **opt-in at build** (the feature is off under
`--no-default-features` and is *not* in `default`). It exposes a soft, runtime-installed hook that
`std/ai` (SP11) consumes to emit GenAI-convention spans, with **neither module Cargo-depending on
the other**.

**Architecture:** A new stdlib module `src/stdlib/telemetry/` (native Rust over `Value`),
registered in both arms of `src/stdlib/mod.rs`, `#[cfg(feature = "telemetry")]`-gated, declared in
`Cargo.toml [features]`. Telemetry state is an **`Interp`-stateful singleton** (mirroring `std/log`:
`Interp` holds the configured pipeline behind interior-mutability cells; emits live to the network
or to a **capture buffer in tests**). Exporters are **hand-rolled over the existing pooled
`reqwest::Client` in `net_http.rs`** — no `opentelemetry`/`sentry`/`posthog` crates (see §6 for the
`!Send`/dep-weight analysis that drives this). Spans/metrics/events are buffered and flushed
asynchronously on a current-thread `spawn_local` task using the take-out-across-await resource
discipline.

**Tech stack:** Rust. Async tree-walker + bytecode VM, both `!Send` (current-thread tokio +
`Rc`/`RefCell`, `LocalSet`). Reqwest (rustls) HTTP client shared with `std/net/http`. JSON
serialization via the existing `stdlib::json` (`to_json_lossy`, total, never panics).

---

## Non-goals (explicitly out of SP12 v1)

- **No gRPC / `http/protobuf` OTLP.** v1 emits OTLP over **HTTP with JSON-encoded protobuf**
  (`/v1/traces`, `/v1/metrics`, `/v1/logs`), the spec-blessed `http/json` protocol. Binary protobuf
  and gRPC are a documented follow-up (they need `prost`/`tonic`, multi-thread assumptions — see §6).
- **No `opentelemetry`/`opentelemetry-otlp`/`sentry`/`posthog-rs` crate dependency.** Hand-rolled
  HTTP exporters only (§6). Adopting the official crates is an explicitly rejected path for v1,
  recorded with rationale, revisitable only if gRPC/full-fidelity is later required.
- **No distributed-trace context propagation across processes** (W3C `traceparent` inject/extract on
  outbound `std/net/http` requests). Spans are correctly parented *within* one AScript process;
  cross-service propagation is a follow-up.
- **No log-signal bridge from `std/log`.** `std/log` stays independent; mirroring `std/log` records
  into OTLP logs is a follow-up. (`telemetry.capture` → OTLP log/event mirroring IS in scope, §4.4.)
- **No sampling configuration** beyond all-on / off; head/tail sampling is a follow-up.
- **Not in `default` features.** Network + new surface → opt-in, matching the proposal's reasoning
  and the `http3`-is-opt-in precedent. Must build clean under `--no-default-features` (feature off).

---

## §1 — Module surface & feature flag

```ascript
import * as telemetry from "std/telemetry"
```

- **Cargo feature `telemetry`** (off by default; NOT in the `default` list). It depends on `data`
  (for `json::to_json_lossy`) and `net` (for the shared reqwest client + the HTTP exporters):
  `telemetry = ["data", "net"]`. No new external crate.
- Module file: `src/stdlib/telemetry/mod.rs` (+ submodules `otlp.rs`, `sentry.rs`, `posthog.rs`,
  `model.rs`). `exports()` returns the builtin bindings; `call(func, args, span)` routes qualified
  calls. Registered behind `#[cfg(feature = "telemetry")]` in BOTH match arms of
  `src/stdlib/mod.rs` and declared `#[cfg(feature = "telemetry")] pub mod telemetry;`.
- **Stateful singleton on `Interp`.** Like `std/log`'s `log_level`/`log_format`/capture buffer,
  `Interp` gains a `telemetry: RefCell<Option<TelemetryState>>` (None = un-initialized = no-op) and a
  capture sink used in tests. Telemetry calls route via `self.call_telemetry(func, args, span)`
  (mirroring `self.call_log`). The same `Interp` instance backs both engines (tree-walker and VM
  delegate stdlib calls identically), so telemetry is **engine-agnostic** — there is **no
  differential concern** (stdlib output is not part of the VM/tree-walker byte-identical corpus
  beyond what `print` already covers; telemetry emits to network/capture, not stdout).

### Zero-cost-when-off guarantee

Absent `telemetry.init(...)`, every telemetry call is a cheap no-op that returns an inert handle
(span/counter/etc. whose methods do nothing). This is the same "safe to leave in production" promise
as `std/log` — code can call `telemetry.startSpan(...)` unconditionally.

---

## §2 — `init` and exporters

```ascript
telemetry.init({
  service: "my-app",                 // service.name resource attribute (required)
  version: "1.4.0",                  // service.version (optional)
  env: "production",                 // deployment.environment.name (optional)
  resource: { "host.name": "web-1" },// extra resource attributes (optional)
  exporters: [
    telemetry.otlp({ endpoint: "http://localhost:4318", protocol: "http/json" }),
    telemetry.sentry({ dsn: env.get("SENTRY_DSN") }),
    telemetry.posthog({ apiKey: env.get("POSTHOG_KEY"), host: "https://us.i.posthog.com" }),
  ],
  flushIntervalMs: 5000,             // batch flush cadence (default 5000)
  maxQueue: 2048,                    // max buffered signals before forced flush (default 2048)
})
```

- `telemetry.otlp(config)`, `telemetry.sentry(config)`, `telemetry.posthog(config)` each return a
  small tagged **exporter descriptor Object** (e.g. `{__exporter: "otlp", endpoint, protocol,
  headers}`) — NOT a native handle and NOT a new `Value` variant (same discipline as `std/schema`'s
  tagged objects). `init` reads the descriptors and builds the live `TelemetryState`.
- **Config resolution / env fallbacks** (each Tier-1 fallible at `init`, never a panic for a missing
  endpoint/DSN — a misconfigured exporter is an operational error):
  - OTLP `endpoint` defaults to `OTEL_EXPORTER_OTLP_ENDPOINT` then `http://localhost:4318`;
    `protocol` is `"http/json"` (the only v1 value; `"http/protobuf"`/`"grpc"` → Tier-2 misuse panic
    "unsupported OTLP protocol; v1 supports http/json"). Per-signal paths: `endpoint + /v1/traces`,
    `/v1/metrics`, `/v1/logs`. Honors `headers:` (e.g. an auth header for Langfuse/Grafana Cloud).
  - Sentry `dsn` defaults to `SENTRY_DSN`. The DSN is parsed into the ingest URL + project +
    public key (the envelope endpoint `https://o…/api/<project>/envelope/`).
  - PostHog `apiKey` defaults to `POSTHOG_KEY`/`POSTHOG_API_KEY`; `host` defaults to
    `https://us.i.posthog.com`. Capture endpoint `host + /capture/`, batch `host + /batch/`.
- **`init` return:** `[ok, err]` — `ok == true` on success, `[nil, err]` if a required exporter
  config is missing/unparseable (Tier-1). **Re-`init` replaces** the pipeline (flushing the old one).
- **`telemetry.flush()`** → `await`able `[ok, err]` forces a flush (called automatically at process
  exit via the existing shutdown path; explicit for tests / before-exit). **`telemetry.shutdown()`**
  flushes and tears the pipeline down to no-op.

> **Langfuse / any OTel backend "just works":** Langfuse, Grafana Tempo, Jaeger, Datadog, etc. all
> ingest OTLP — the `otlp` exporter with the backend's endpoint + auth header covers them with no
> per-vendor code. This is why SP11's GenAI spans need no Langfuse-specific exporter.

---

## §3 — Tracing (spans)

```ascript
// Explicit span, manual lifecycle:
let span = telemetry.startSpan("handle-request", { attributes: { route: "/users" } })
span.setAttribute("user.id", uid)
span.addEvent("cache-miss", { key: cacheKey })
span.setStatus("ok")                 // "ok" | "error" | "unset"
span.end()

// Scoped helper: times, auto-ends, records a thrown Tier-2 panic as error status,
// returns the callback's value as a Tier-1 pair (a recovered panic → [nil, err]):
let [result, err] = await telemetry.span("db-query", async fn() {
  return await db.query("SELECT ...")
})
```

- **Span handle.** `telemetry.startSpan(name, opts?)` returns a `Value::Native` span handle (state in
  `Interp.resources` as a `ResourceState::TelemetrySpan` — span id, trace id, parent id, start time,
  attributes, events, status). Methods: `setAttribute(k, v)`, `addEvent(name, attrs?)`,
  `setStatus(status, message?)`, `end()`. Calling a method after `end()` is a no-op (not a panic).
- **Parenting via a current-span stack.** A thread-local current-span stack (analogous to the
  generator stack in `coro.rs`) is pushed by `telemetry.span(cb)` for the duration of `cb` and by an
  explicit `span` between `startSpan`/`end` only when used through the scoped form. A span created
  while another is current parents to it; root spans mint a new trace id. (Manual `startSpan` spans
  parent to the current scoped span if one is active, else are roots — documented, deterministic.)
- **`telemetry.span(name, fn)`** scoped helper: starts a span, pushes it as current, drives the
  callback (sync or `async fn`, awaited), auto-ends, sets `error` status + records the message if the
  callback's body panics (caught like `recover`), and returns `[value, err]`. **Never** swallows the
  value on success (`[value, nil]`). This is the SP11 GenAI-span analog and the 90% ergonomic path.
- **Span → exporter mapping:** OTLP — a span becomes an OTLP `Span` JSON object (hex `traceId`/
  `spanId`, `startTimeUnixNano`/`endTimeUnixNano`, `attributes` as OTLP KeyValue, `events`,
  `status`). Sentry — a root span + its subtree becomes a Sentry **transaction** envelope; an
  `error`-status span additionally emits a Sentry error event. PostHog — spans are NOT sent to
  PostHog (PostHog is events-only).

### Soft hook for SP11 (the load-bearing contract)

`Interp` exposes an **internal hook** that `std/ai` calls without a Cargo dependency on telemetry:

```rust
// On Interp (always present; cfg-independent signature so std/ai compiles with telemetry OFF):
impl Interp {
    /// Returns true iff telemetry is initialized AND tracing is enabled.
    pub(crate) fn telemetry_active(&self) -> bool;
    /// Start a span through the telemetry pipeline if active; returns an opaque span id.
    /// No-op returning None when telemetry is absent/off (so std/ai never branches on a feature).
    pub(crate) fn telemetry_span_start(&self, name: &str, attrs: Vec<(String, Value)>) -> Option<u64>;
    pub(crate) fn telemetry_span_set(&self, id: u64, key: &str, val: Value);
    pub(crate) fn telemetry_span_event(&self, id: u64, name: &str, attrs: Vec<(String, Value)>);
    pub(crate) fn telemetry_span_end(&self, id: u64, status: SpanStatus);
}
```

- **Soft, runtime-optional, both-build-independently.** These methods are **defined on `Interp`
  unconditionally** but their *body* is `#[cfg(feature = "telemetry")]`-bridged: with the feature
  ON, they delegate to `TelemetryState`; with it OFF, they are inert (`telemetry_active → false`,
  `telemetry_span_start → None`). `std/ai` calls them with no `cfg` of its own and no telemetry
  import — so **`std/ai` builds with telemetry absent** and **`std/telemetry` builds with ai
  absent**, satisfying the owner's soft-hook decision. (The `SpanStatus` enum lives in a core
  module, not behind the feature.)
- **Tracing is OPT-IN: it activates only when telemetry is initialized.** With no `telemetry.init`,
  `telemetry_active()` is false and SP11 emits nothing. This is the owner's decision #3.

---

## §4 — Metrics & events

### 4.1 Metrics

```ascript
let reqs = telemetry.counter("http.requests", { unit: "1", description: "..." })
reqs.add(1, { route: "/users", method: "GET" })

let lat = telemetry.histogram("http.latency", { unit: "ms" })
lat.record(12.4, { route: "/users" })

let inflight = telemetry.gauge("http.inflight")
inflight.set(7)
```

- Counter / histogram / gauge are `Value::Native` instrument handles (state in `Interp.resources`:
  name, unit, kind, accumulated data-points keyed by attribute set). Aggregated in-process and
  exported as OTLP **Metrics** (`Sum`/`Histogram`/`Gauge`) on each flush. Metrics go to OTLP only
  (not Sentry/PostHog) in v1.
- Re-fetching the same name returns the same instrument (idempotent registration).

### 4.2 Event capture (analytics)

```ascript
telemetry.capture("signup_completed", {
  distinctId: userId,
  properties: { plan: "pro", referrer: "hn" },
})
telemetry.identify(userId, { email: "a@b.com", plan: "pro" })
```

- `capture(event, opts)` / `identify(distinctId, props)` map to the **PostHog** `/capture/` +
  `/batch/` HTTP API (`api_key` + `distinct_id` + `event` + `properties`; `$identify` for
  `identify`). Batched, flushed on interval/size.
- **OTLP mirroring (config flag):** `init({ mirrorEventsToOtlp: true })` *additionally* emits each
  `capture` as an OTLP **log record** (an event). Default OFF — events go to PostHog by default
  (proposal §4.4). With no PostHog exporter configured and mirroring off, `capture` is a no-op.
- Both return `[ok, err]`-shaped acknowledgements only on explicit `await telemetry.flush()`; the
  call itself enqueues (fire-and-forget, returns `nil`).

---

## §5 — Error model (Tier-1 vs Tier-2)

| Situation | Result |
|---|---|
| Exporter network/TLS/HTTP-4xx-5xx failure on flush; missing required exporter config at `init` (no endpoint/DSN/key) | **Tier-1** `[…, err]` (from `init`/`flush`) — never aborts the program; a failed flush is logged once to stderr and dropped |
| Wrong argument *types* (`init` not an object, `counter` name not a string, unknown exporter kind, unsupported OTLP protocol, `setStatus` not in the enum) | **Tier-2** panic (programmer error) |
| Any telemetry call before `init` | **No-op** (inert handle), never an error |

A telemetry failure **must never take down the user's program** — it is observability, not business
logic. Network failures during flush are swallowed (best-effort) after a single stderr warning,
consistent with how production telemetry SDKs behave.

---

## §6 — Crate choice & the `!Send` analysis (the critical decision)

**Decision: hand-roll all three exporters over the existing reqwest client. Do NOT depend on
`opentelemetry`/`opentelemetry-otlp`, `sentry`, or `posthog-rs` for v1.** Rationale, grounded in the
runtime constraint:

- **The runtime is `!Send`, current-thread tokio + `Rc`/`RefCell`, on a `LocalSet`.** The official
  observability crates are built for the *opposite* model:
  - **`opentelemetry-otlp` / `opentelemetry_sdk`** spawn their batch processor on a runtime. The
    current-thread story (`rt-tokio-current-thread`) **spins up a separate background runtime/thread
    for export** and there are documented `tokio::spawn` **hang** issues when an OTLP layer is mixed
    with task spawning. Its types and the global `TracerProvider` assume `Send + Sync`. Bridging this
    into our single-thread `LocalSet` is fragile and pulls a large dep tree (`prost`, `tonic`/`http`,
    `tower`).
  - **`sentry`** wants the client initialized **before** the async runtime starts and runs a
    background transport **thread**; the docs explicitly advise against `#[tokio::main]`. That
    conflicts directly with our `#[tokio::main(flavor = "current_thread")]` + `LocalSet` entry
    points and the per-`Interp` lifecycle.
  - **`posthog-rs`** is a thin HTTP client whose async client assumes a `Send` runtime.
- **What we actually need is small and HTTP-only.** OTLP `http/json` is a documented POST of
  JSON-encoded protobuf to `/v1/{traces,metrics,logs}` (hex `traceId`/`spanId`, proto3 JSON
  mapping). Sentry ingest is a POST of a newline-delimited **envelope** to the DSN's
  `/api/<project>/envelope/`. PostHog is a POST to `/capture/` + `/batch/`. All three are **plain
  JSON-over-HTTPS** that our tuned, pooled `reqwest::Client` (rustls, in `net_http.rs`) sends
  natively on the current-thread runtime via `spawn_local`, using the take-out-across-await resource
  discipline — **zero new crate, zero `!Send` bridging, small dep tree** (consistent with the repo's
  stated bias and the `http3`-opt-in precedent).
- **Wire-format fidelity is bounded and testable.** We hand-write the OTLP-JSON shaping (a few
  hundred lines, validated against recorded fixtures and a local collector in the env-gated suite).
  This is the same trade the proposal recommends (§4.5) and matches how `std/ai` (SP11) hand-shapes
  provider JSON.

**Residual risk / revisit trigger:** if a consumer later needs **gRPC OTLP**, **binary protobuf**,
or **full OTel SDK semantics** (views, exemplars, baggage propagation), revisit adopting
`opentelemetry-otlp` behind a *separate* opt-in feature with a dedicated worker thread bridging the
`!Send` boundary. Documented as a follow-up, not a v1 commitment.

> Contrast with SP11: there the owner's decision is to **wrap a crate** (`genai`), accepting the
> `!Send` bridging cost because reimplementing ~25 provider protocols + SigV4 + Vertex OAuth natively
> is infeasible. Here the exporters are 3 trivial HTTP shapes, so hand-rolling is strictly the
> lighter, safer path. Both decisions follow the same principle: wrap only when the native cost is
> high; the costs differ by module.

---

## §7 — Testing strategy (no network, no secrets by default)

- **Injectable HTTP-send seam.** The exporters call an internal `send(spec) -> Result<…>` trait
  object. In `cargo test`, telemetry runs in **capture mode** (mirroring `std/log`'s capture sink):
  the seam records the exact OTLP/Sentry/PostHog HTTP request bodies into an in-`Interp` buffer
  readable from Rust (`Interp::telemetry_capture()`), so unit tests assert the produced JSON without
  any socket. No default test ever opens a connection or reads a secret.
- **Unit tests (default `cargo test`, feature on):** drive `.as` programs through `run_source`
  (capture sink); assert the captured exporter payloads. Cover: span tree → OTLP spans (parenting,
  attributes, status, events); `telemetry.span` happy path + panic→error-status; counter/histogram/
  gauge → OTLP metrics; `capture`/`identify` → PostHog payload; Sentry transaction + error envelope;
  no-op-when-uninitialized; re-`init` replaces; Tier-2 misuse panics.
- **Env-gated live suite (excluded from CI by default):** behind `ASCRIPT_TELEMETRY_LIVE=1`, point
  `otlp` at a locally spun OTel collector / Sentry-relay / PostHog test project (or the existing
  `--features net` local-mock-server pattern in `tests/`). Never runs without the env var; never in
  default CI.
- **Both feature configs:** `cargo test` (telemetry OFF by default since it's not in `default`) must
  pass; a dedicated `cargo test --features telemetry` job exercises the module; `--no-default-features`
  must build (feature absent → module cfg'd out cleanly). Clippy clean in all configs,
  `await_holding_refcell_ref` denied + clean (exporters use take-out-across-await).

---

## §8 — File-touch map (for the plan)

| Area | Files |
|---|---|
| New module | `src/stdlib/telemetry/{mod,model,otlp,sentry,posthog}.rs` |
| Routing | `src/stdlib/mod.rs` (both `std_module_exports` + `call` arms, `#[cfg(feature="telemetry")]`) |
| Interp state + hook | `src/interp.rs` (`telemetry: RefCell<Option<TelemetryState>>`, `call_telemetry`, the `telemetry_*` soft-hook methods + `SpanStatus`, capture sink, current-span thread-local, flush-on-exit) |
| Resources | `src/interp.rs` (`ResourceState::Telemetry{Span,Counter,Histogram,Gauge}` + accessors) |
| Cargo | `Cargo.toml` (`telemetry = ["data","net"]`, NOT in `default`) |
| Tests | `tests/telemetry.rs` (capture-mode unit + env-gated live), inline `#[tokio::test]` in the module |
| Docs | `docs/content/stdlib/telemetry.md` (new), README stdlib table, `docs/content/` index/nav |

---

## §9 — Open questions (carried to review, none blocking)

1. **Sentry transaction fidelity.** v1 maps a span subtree → one Sentry transaction with child
   spans; deeply nested async spans may need trace-context plumbing. Acceptable v1 limitation;
   confirm at review whether error-only Sentry (drop transactions) is simpler for v1.
2. **Metric temporality.** OTLP wants delta vs cumulative aggregation temporality declared. v1 ships
   **cumulative** (the OTLP default, broadest backend support); revisit if a backend needs delta.
3. **Current-span stack across `await` boundaries.** The thread-local stack must be saved/restored
   around `spawn_local` task boundaries so a span started in one task doesn't leak as the parent of
   an unrelated concurrent task. The plan's Phase 1 nails this with a differential-style concurrency
   test; flagged here as the subtle correctness point.
