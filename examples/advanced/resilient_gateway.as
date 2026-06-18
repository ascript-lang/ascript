// resilient_gateway.as — a production-shaped backend gateway built on
// std/resilience: trace propagation, a policy-wrapped route, an explicitly
// composed deadline -> breaker -> retry backend call, /metrics + /readyz
// endpoints, and the global-state actor pattern.
//
// This example is SELF-CONTAINED and runs to completion: it binds an in-process
// HTTP server on an EPHEMERAL port (never printed), drives it with an in-process
// client, and stops after a fixed number of requests via `maxRequests`. The
// output is verdicts/status codes only — byte-identical across all four engines
// (tree-walker, specialized VM, generic VM, compiled .aso).
//
// IMPORTANT — per-isolate honesty (spec §7): every policy's state (breaker
// windows, token buckets, the metrics registry, task-local deadlines/trace ids)
// lives in ONE isolate. Under `server.serve({workers: N})` there are N
// independent copies — usually correct (per-replica circuit breaking is the
// deployed norm). When state MUST be process-global, use a `worker class` actor
// (see GlobalLimiter below): one isolate owns the policy, everyone else asks it.
//
//   ascript run examples/advanced/resilient_gateway.as
import * as server from "std/http/server"
import { get } from "std/net/http"
import * as resilience from "std/resilience"
import * as task from "std/task"
import * as string from "std/string"

const HOST = "127.0.0.1"

// ── §7.2 — global state via a `worker class` actor ──────────────────────────
// A strict process-global rate limit can't live in a per-isolate policy. The
// documented pattern is a dedicated actor: one isolate OWNS the limiter, everyone
// else asks it over the FIFO mailbox. Honest trade-off: every check is a mailbox
// round-trip — use it for LOW-QPS global decisions, per-isolate policies for the
// hot path.
worker class GlobalLimiter {
  lim: any? = nil
  fn init() {
    self.lim = resilience.limiter({capacity: 2, refillPerSec: 0.001})
  }
  async fn tryAcquire(): bool {
    return self.lim.tryAcquire()
  }
}

// ── §3.5 — explicit deadline -> breaker -> retry -> fetch composition ────────
// Wrap order matters (the spec carries both diagrams verbatim):
//
//   deadline (OUTERMOST)  — one total budget for the whole operation; expiry
//                           mid-backoff cancels retrying ("timeout outside retry").
//     breaker             — sees the inner composite as ONE operation: an
//                           exhausted retry sequence counts as a single failure.
//       retry             — re-attempts the flaky call; `retryIf` skips retrying a
//                           `breaker-open` rejection (the published idiom — never
//                           hammer an open breaker).
//         fetchBackend    — the actual dependency call.
async fn composedBackendCall() {
  let b = resilience.breaker({name: "backend", failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
  let retryP = resilience.retry({attempts: 3, baseMs: 1, retryOn: "error", retryIf: (e) => e.code != "breaker-open"})
  let attempts = [0]
  async fn fetchBackend() {
    attempts[0] = attempts[0] + 1
    // Flaky: fails once (retried), then succeeds.
    if (attempts[0] < 2) {
      return [nil, {message: "transient upstream error", code: "err"}]
    }
    return "payload"
  }
  // deadline(outer) -> breaker -> retry(deadline-aware) -> fetch
  let [v, err] = await resilience.deadline(1000, async () => {
    let [inner, ierr] = await b.call(async () => await retryP.call(fetchBackend))
    if (ierr != nil) {
      return [nil, ierr]
    }
    return inner
  })
  print(v) // payload
  print(err) // nil
  print(attempts[0]) // 2 — failed once, retried, succeeded
}

// ── The gateway server: trace middleware + a policy-wrapped route ────────────
// Per-isolate hot-path policies (the common case).
let perClient = resilience.limiter({capacity: 1, refillPerSec: 0.001, name: "quotes"})
let edgeBreaker = resilience.breaker({name: "quotes", failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})

// The route handler reads the ambient trace id set by the middleware.
fn quote(req) {
  let tid = resilience.traceId() ?? "none"
  return {status: 200, headers: {"x-trace": tid}, body: "quote-ok"}
}

fn pingDb() {
  return true
}

let app = server.create()

// Trace middleware: take the request id from a header, falling back to a FIXED
// id (a real gateway would use `uuid.v4()`, but a fixed id keeps this example's
// output byte-identical across runs). `withTrace` scopes the id so the handler
// and any downstream log/telemetry records pick it up automatically.
app.use((req, next) => {
  let id = req.headers["x-request-id"] ?? "fixed-trace"
  return resilience.withTrace(id, () => next(req))
})

// Health/readiness endpoints (spec §6.3): liveness always 200; readiness runs
// the registered checks and reports 200/ok or 503/degraded.
app.route("GET", "/healthz", resilience.health({}))
app.route("GET", "/readyz", resilience.health({checks: {db: pingDb}}))

// Prometheus metrics (spec §6.2): the per-isolate registry rendered as text.
app.route("GET", "/metrics", resilience.metricsHandler())

// The policy-wrapped route (spec §6.4): the wrapper applies, OUTERMOST first,
// the limiter -> breaker -> deadline, then the handler — and maps rejection codes
// to HTTP statuses (rate-limited -> 429, breaker-open/bulkhead-full -> 503,
// deadline-exceeded -> 504). One line for the whole 429/503/504 ladder.
app.route("GET", "/quote", resilience.handler({limiter: perClient, breaker: edgeBreaker, deadlineMs: 500}, quote))

async fn runServer() {
  // Serve exactly the number of requests the client below makes, then drain.
  await app.serve({maxRequests: 5})
}

async fn main() {
  // First, the global-state and composition demos (no server needed).
  let gl = await GlobalLimiter.spawn()
  print(await gl.tryAcquire()) // true  — first global token
  print(await gl.tryAcquire()) // true  — second global token
  print(await gl.tryAcquire()) // false — global limit reached
  gl.close()
  await composedBackendCall()

  // Now the gateway: bind an ephemeral port (never printed) and self-drive it.
  let [bound, berr] = await app.bind(HOST, 0)
  if (berr != nil) {
    print(`bind failed: ${berr.message}`)
    return
  }
  const base = `http://${HOST}:${bound}`
  let serving = task.spawn(runServer())

  // 1. Liveness.
  let [r1, _e1] = await get(`${base}/healthz`)
  print(r1.status) // 200

  // 2. Readiness (the db check passes).
  let [r2, _e2] = await get(`${base}/readyz`)
  print(r2.status) // 200

  // 3. First /quote consumes the single token → 200, with the trace id echoed.
  let [r3, _e3] = await get(`${base}/quote`)
  print(r3.status) // 200
  print(r3.headers["x-trace"]) // fixed-trace — the middleware id reached the handler

  // 4. Second /quote is rate-limited → 429 (the wrapper maps the limiter verdict).
  let [r4, _e4] = await get(`${base}/quote`)
  print(r4.status) // 429

  // 5. Metrics endpoint exports the per-isolate registry as Prometheus text.
  let [r5, _e5] = await get(`${base}/metrics`)
  print(r5.status) // 200
  let [body, _be] = await r5.text()
  print(string.contains(body, "limiter") || string.contains(body, "breaker")) // true
  await serving
}

await main()

print("resilient_gateway ok")
