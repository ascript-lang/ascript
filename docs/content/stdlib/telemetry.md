:::eyebrow Standard library

# Telemetry & observability

`std/telemetry` is a thin, vendor-neutral observability facade: **tracing spans**,
**metrics** (counter / histogram / gauge), and **analytics events**
(`capture` / `identify`), delivered through three hand-rolled exporters —
**OTLP** (HTTP/JSON), **Sentry**, and **PostHog**. Import it as a namespace:
`import * as telemetry from "std/telemetry"`.

Telemetry is **opt-in twice over**:

- **At runtime** — every call is a cheap no-op until `telemetry.init(...)` runs, so
  `telemetry.startSpan(...)` / `telemetry.counter(...)` are safe to leave in
  production code that may run without a configured backend.
- **At build** — it lives behind the `telemetry` Cargo feature, which is **not in
  `default`** (it depends on `data` + `net`). Build a telemetry-enabled binary
  with `cargo build --features telemetry`; under `--no-default-features` (or any
  build without the feature) the module is absent.

> The official `opentelemetry` / `sentry` / `posthog` crates assume a `Send`,
> multi-thread runtime; AScript's interpreter is `!Send` (current-thread tokio +
> `Rc`/`RefCell`). The three exporters are therefore **hand-rolled JSON-over-HTTP**
> on the same pooled `reqwest` client `std/net/http` uses — lighter, and with no
> `!Send` bridging.

## init and exporters

`telemetry.init(config)` builds the live pipeline. It returns a `[ok, err]` pair:
`ok` is `true` on success; a missing or unparseable required exporter config (no
endpoint / DSN / key) is a Tier-1 `[nil, err]` — a misconfigured exporter is an
operational error, never a panic.

```ascript
import * as telemetry from "std/telemetry"

let [ok, err] = telemetry.init({
  service: "my-app",                 // service.name resource attribute (required)
  version: "1.4.0",                  // service.version (optional)
  env: "production",                 // deployment.environment.name (optional)
  resource: { "host.name": "web-1" },// extra resource attributes (optional)
  exporters: [
    telemetry.otlp({ endpoint: "http://localhost:4318", protocol: "http/json" }),
    telemetry.sentry({ dsn: env.get("SENTRY_DSN") }),
    telemetry.posthog({ apiKey: env.get("POSTHOG_KEY") }),
  ],
})
```

Each exporter constructor returns a small tagged descriptor object that `init`
reads:

- **`telemetry.otlp({ endpoint?, protocol?, headers? })`** — `endpoint` defaults to
  `OTEL_EXPORTER_OTLP_ENDPOINT`, then `http://localhost:4318`. The only supported
  `protocol` is `"http/json"` (passing `"http/protobuf"` / `"grpc"` is a Tier-2
  programmer error). Per-signal paths are `endpoint + /v1/traces`, `/v1/metrics`,
  `/v1/logs`. Any backend that ingests OTLP (Langfuse, Grafana Tempo, Jaeger,
  Datadog, …) works with just its endpoint plus an auth header in `headers`.
- **`telemetry.sentry({ dsn? })`** — `dsn` defaults to `SENTRY_DSN`. The DSN is
  parsed into the envelope ingest URL and auth key.
- **`telemetry.posthog({ apiKey?, host? })`** — `apiKey` defaults to `POSTHOG_KEY`
  then `POSTHOG_API_KEY`; `host` defaults to `https://us.i.posthog.com`.

**Re-`init`** replaces the pipeline, flushing the previous one first.
**`telemetry.shutdown()`** flushes and tears the pipeline down to a no-op again.
**`telemetry.flush()`** forces an immediate export (it also runs automatically at
process exit); both are `await`able and return `[ok, err]`.

## Tracing — spans

A span has a name, a start/end time, attributes, timestamped events, and a status
(`"ok"` / `"error"` / `"unset"`).

### Manual lifecycle

```ascript
let span = telemetry.startSpan("handle-request", { attributes: { route: "/users" } })
span.setAttribute("user.id", "u-42")
span.addEvent("cache-miss", { key: "users:u-42" })
span.setStatus("ok")              // "ok" | "error" | "unset"
span.end()
```

Calling a method **after `end()` is a no-op**, not an error.

### Scoped helper

`telemetry.span(name, fn)` times the callback (sync or `async`), auto-ends the
span, records a thrown Tier-2 panic as `error` status, and returns the callback's
result as a `[value, err]` pair (a recovered panic → `[nil, err]`, and the program
continues):

```ascript
let [result, err] = await telemetry.span("db-query", async () => {
  return await db.query("SELECT ...")
})
```

### Parent / child

A span created while another is current **parents to it** (same trace id, the
child's parent is the enclosing span); a span created at the top level is a trace
**root** with a fresh trace id. The current span is tracked per async task, so
spans started in concurrent tasks never cross-parent.

### Exporter mapping

- **OTLP** — each span becomes an OTLP `Span` (hex `traceId` / `spanId`,
  `startTimeUnixNano` / `endTimeUnixNano` as strings, attributes as `KeyValue`,
  events, status).
- **Sentry** — a trace's root span and its subtree become one Sentry
  **transaction** envelope; an `error`-status span additionally emits a Sentry
  **error event**.
- **PostHog** — spans are not sent to PostHog (it is events-only).

## Metrics

Counter, histogram, and gauge instruments aggregate in-process (cumulative
temporality) and export as OTLP metrics on each flush. Re-fetching the same name
returns the **same instrument**.

```ascript
let requests = telemetry.counter("http.requests", { unit: "1" })
requests.add(1, { route: "/users", method: "GET" })

let latency = telemetry.histogram("http.latency", { unit: "ms" })
latency.record(12.4, { route: "/users" })

let inflight = telemetry.gauge("http.inflight")
inflight.set(7)
```

Metrics go to OTLP only (not Sentry / PostHog) in v1.

## Analytics events

`capture` / `identify` map to the PostHog `/batch/` HTTP API. With no PostHog
exporter configured (and `mirrorEventsToOtlp` off) they are a no-op.

```ascript
telemetry.capture("signup_completed", {
  distinctId: userId,
  properties: { plan: "pro", referrer: "hn" },
})
telemetry.identify(userId, { email: "a@b.com", plan: "pro" })
```

Setting `init({ mirrorEventsToOtlp: true })` additionally emits each `capture` as
an OTLP **log record**.

## Error model

| Situation | Result |
| --- | --- |
| Exporter network / HTTP failure on flush; missing required exporter config at `init` | **Tier-1** `[…, err]` — never aborts the program; a failed flush is logged once to stderr and dropped |
| Wrong argument *types* (`init` not an object, unknown exporter kind, unsupported OTLP protocol, `setStatus` not in the enum) | **Tier-2** panic (programmer error) |
| Any telemetry call before `init` | **No-op** (inert handle), never an error |

A telemetry failure must never take down your program — it is observability, not
business logic.

## Relationship to `std/ai`

`std/ai` (the AI client) emits OpenTelemetry **GenAI-convention** spans through an
internal hook this module installs. That tracing is opt-in: it is active only once
`telemetry.init(...)` has run, and emits nothing otherwise. Neither module depends
on the other at build time.

## Running the example

```bash
cargo run --features telemetry -- run examples/advanced/telemetry.as
```
