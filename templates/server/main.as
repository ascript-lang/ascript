// AScript server template — graceful shutdown, healthcheck, container-ready.
//
// A minimal, production-shaped HTTP service: a /healthz liveness probe, a root
// route, a resilient upstream-proxy route, and a clean SIGTERM/SIGINT drain so
// orchestrators (Docker, Kubernetes) can stop the container gracefully.
//
//   ascript run main.as            # run locally (PORT env or 8080)
//   ascript build --native main.as -o app && ./app
//
// See README.md for containerizing (the Dockerfile is a multi-stage native build).
import * as server from "std/http/server"
import * as process from "std/process"
import * as task from "std/task"
import * as env from "std/env"
import * as log from "std/log"
import * as time from "std/time"

let started = time.monotonic()
let srv = server.create()

// Liveness/health probe — orchestrators poll this. Returns JSON.
srv.route("GET", "/healthz", (req) => {
  return {status: 200, headers: {"content-type": "application/json"}, body: `{"ok":true,"uptimeMs":${time.monotonic() - started}}`}
})

// Root route.
srv.route("GET", "/", (req) => {
  return {status: 200, body: "hello from ascript\n"}
})

// A resilient upstream call: retry the flaky dependency a few times with
// exponential backoff before giving up with a 502.
//
// `fetchUpstream` returns a `[value, err]` pair (the std/net/http convention).
// `task.retry({retryOn: "error"})` re-invokes it while it yields an error pair;
// on success it returns the bare success value, on exhaustion the last error pair.
//
// UPGRADE POINT (§9.3): swap this hand-rolled task.retry for a composed
// std/resilience policy (deadline -> breaker -> retry) once you need circuit
// breaking / per-client rate limits — see examples/advanced/resilient_gateway.as.
async fn fetchUpstream() {
  // Replace with a real `import { get } from "std/net/http"` call, e.g.
  //   let [resp, e] = await get("https://upstream.example/health")
  //   if (e != nil) { return [nil, e] }
  //   return [await resp.text(), nil]
  return ["ok\n", nil]
}

srv.route("GET", "/proxy", async (req) => {
  let [v, err] = await task.retry(fetchUpstream, {attempts: 3, baseMs: 100, backoff: "exponential", retryOn: "error"})
  if (err != nil) {
    return {status: 502, headers: {"content-type": "application/json"}, body: `{"error":"${err.message}"}`}
  }
  return {status: 200, body: v}
})

// Graceful shutdown: on SIGTERM/SIGINT, arm the drain. In-flight requests finish
// (up to drainTimeout), no new connections are accepted, then serve() returns.
// The handler receives the signal name as its argument.
process.on("SIGTERM", (sig) => srv.shutdown())
process.on("SIGINT", (sig) => srv.shutdown())

let host = env.get("HOST") ?? "0.0.0.0"
let port = int(env.get("PORT") ?? "8080")!

// Bind first so we surface a clear error if the port is taken, then serve.
let [bound, berr] = await srv.bind(host, port)
if (berr != nil) {
  log.error("bind failed", {error: berr.message})
  exit(1)
}
log.info("listening", {host: host, port: bound})

let [_, err] = await srv.serve({onShutdown: () => log.info("drain started"), drainTimeout: 8000})
if (err != nil) {
  log.error("serve failed", {error: err.message})
}
log.info("stopped")
