// server_multicore.as — a multi-core HTTP server on the shared read-only heap.
//
// `server.serve({ workers: N })` spreads the accept loop across N shared-nothing
// isolates that each bind the SAME port via SO_REUSEPORT, so the kernel
// load-balances connections across cores (the nginx/Envoy/Node-cluster model).
// `workers: 0` resolves to num_cpus.
//
// The two server-tier pieces working together:
//
//   1. Shared read-only heap — build the big read-only state ONCE on the main
//      isolate and `shared.freeze` it. Each isolate receives the frozen graph as an
//      Arc pointer bump (NOT a per-isolate deep copy), so a 5 MB routing table costs
//      one atomic increment per isolate, not size × num_cpus of RAM.
//
//   2. Per-isolate `setup` — a `worker fn` shipped to and run IN each isolate at
//      boot. It opens this isolate's OWN per-isolate resources (a DB pool, prepared
//      statements — none cross the airlock) and registers handlers, then returns the
//      isolate's server handle. Handlers are `worker fn`s.
//
// On Windows (no SO_REUSEPORT) this transparently falls back to a single isolate
// with a one-time warning — correct, just single-core. (That branch can't be shown
// from an .as file, which runs identically on every platform; it is covered by the
// runtime's #[cfg(windows)] test.)
//
// This server runs forever (one accept loop per isolate). Drive it from another
// process:
//
//   ascript run examples/advanced/server_multicore.as     # terminal 1 (blocks)
//   curl localhost:8088/config                            # terminal 2
//   curl localhost:8088/routes/GET%20%2Fusers
//
// Pass { maxRequests: N } to serve to stop the whole group after N total requests.
import * as server from "std/http/server"
import * as shared from "std/shared"
import * as json from "std/json"

const HOST = "127.0.0.1"
const PORT = 8088

// Build the big read-only state ONCE, on the main isolate, then freeze it. In a real
// service this is a routing table / feature-flag snapshot / geo-IP database that is
// expensive to build and must be READ but never mutated by a request handler.
fn buildConfig() {
  return {region: "us-east-1", routes: {"GET /users": "listUsers", "GET /users/:id": "getUser", "POST /users": "createUser"}, limits: {maxBody: 1048576, ratePerMin: 600}}
}

let config = shared.freeze(buildConfig()) // immutable, Send, zero-copy across isolates

// The per-isolate setup: runs IN each isolate at boot. `config` crosses as an Arc
// pointer bump. Per-isolate native resources (a DB pool) would be opened HERE, inside
// the isolate, and never cross the airlock.
worker fn boot(cfg) {
  let app = server.create()

  // A handler reading the frozen shared config (zero-copy). Scalar reads off the
  // frozen graph materialize to plain values, so we assemble an ordinary response
  // object from them. (A frozen SUB-object is itself a Shared view — read its scalar
  // leaves rather than embedding the frozen node directly.)
  app.route("GET", "/config", (req) => {
    let view = {region: cfg.region, maxBody: cfg.limits.maxBody, ratePerMin: cfg.limits.ratePerMin}
    let [body, err] = json.stringify(view)
    if (err != nil) {
      return {status: 500, body: `serialize: ${err.message}`}
    }
    return {status: 200, headers: {"content-type": "application/json"}, body: body}
  })

  // A keyed lookup into the frozen routing table.
  app.route("GET", "/routes/:key", (req) => {
    let handler = cfg.routes[req.params.key]
    if (handler == nil) {
      return {status: 404, body: `no route ${req.params.key}`}
    }
    return {status: 200, body: handler}
  })
  app.route("GET", "/health", (req) => {
    return {status: 200, body: "ok"}
  })
  return app // this isolate's OWN server handle
}

async fn main() {
  print(`serving on http://${HOST}:${PORT} across all cores (Ctrl-C to stop)`)
  // workers: 0 = num_cpus. Each isolate runs boot() then accepts on its own
  // SO_REUSEPORT socket. The frozen `config` is shared by pointer, not copied.
  let [_, err] = await server.serve({port: PORT, host: HOST, workers: 0, setup: boot, args: [config]})
  if (err != nil) {
    print(`server error: ${err.message}`)
  }
}

await main()
