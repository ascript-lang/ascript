// std/telemetry — vendor-neutral observability: tracing spans, metrics, and
// analytics events through hand-rolled OTLP / Sentry / PostHog exporters.
//
// Telemetry is OPT-IN at runtime (a no-op until `telemetry.init`) and OPT-IN at
// build (the `telemetry` Cargo feature, not in `default`). Run with a telemetry-
// enabled binary:
//
//   cargo run --features telemetry -- run examples/advanced/telemetry.as
//
// The OTLP exporter here points at the conventional local collector endpoint
// (http://localhost:4318). With no collector listening the flush fails silently
// (a telemetry failure is logged once and dropped — it never aborts the program),
// so this example is deterministic and prints the same lines regardless.

import * as telemetry from "std/telemetry"

// Before init, every telemetry call is an inert no-op — safe to leave in code.
telemetry.startSpan("warmup").end()

// Initialize the pipeline: a service name (the OTLP `service.name`) plus an OTLP
// HTTP/JSON exporter. Returns [ok, err]; a missing/unparseable config is Tier-1.
let [ok, initErr] = telemetry.init({
  service: "telemetry-example",
  version: "1.0.0",
  env: "demo",
  exporters: [
    telemetry.otlp({ endpoint: "http://localhost:4318", protocol: "http/json" }),
  ],
})
print(`init ok: ${ok}`)

// Scoped span: times the callback, auto-ends, records a panic as error status,
// and returns [value, err]. The happy path returns the callback's value.
let [sum, spanErr] = await telemetry.span("compute", async () => {
  let total = 0
  for (n in 1..5) {
    total = total + n
  }
  return total
})
print(`compute = ${sum}`)
print(`compute err is nil: ${spanErr == nil}`)

// Manual span with attributes, an event, and an explicit status.
let req = telemetry.startSpan("handle-request", { attributes: { route: "/users" } })
req.setAttribute("user.id", "u-42")
req.addEvent("cache-miss", { key: "users:u-42" })
req.setStatus("ok")
req.end()

// Metrics: a cumulative counter and a histogram, aggregated in-process and
// exported as OTLP metrics on flush.
let requests = telemetry.counter("http.requests", { unit: "1" })
requests.add(1, { route: "/users", method: "GET" })
requests.add(1, { route: "/users", method: "GET" })

let latency = telemetry.histogram("http.latency", { unit: "ms" })
latency.record(12.5, { route: "/users" })
latency.record(7.5, { route: "/users" })

// Force a flush now (normally automatic at process exit). A network failure is
// swallowed, so we ignore the [ok, err] result for deterministic output.
await telemetry.flush()

// Tear the pipeline down: subsequent calls become no-ops again.
await telemetry.shutdown()
telemetry.startSpan("after-shutdown").end()  // no-op

print("telemetry ok")
