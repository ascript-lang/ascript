// PROFILE TARGET (PERF campaign blind spot): server-request-shaped glue WITHOUT
// sockets (deterministic, run-to-completion). Per request: parse JSON, route, build, stringify.
// NOTE: routes is an Object (not Map); indexing with a dynamic key returns nil for misses,
// so ?? nil-coalescing dispatches to handle_missing.
import * as json from "std/json"
import * as time from "std/time"

fn handle_get(req) { return { status: 200, body: { id: req.id, ok: true } } }
fn handle_put(req) { return { status: 200, body: { id: req.id, saved: req.payload } } }
fn handle_missing(req) { return { status: 404, body: { error: "no route" } } }

let routes = { "GET /item": handle_get, "PUT /item": handle_put }

let t0 = time.monotonic()
let bytes = 0
for (i in 0..500000) {
  let raw = `{"method":"${i % 2 == 0 ? "GET" : "PUT"}","path":"/item","id":${i},"payload":"p${i % 50}"}`
  let [req, e1] = json.parse(raw)
  let key = `${req.method} ${req.path}`
  let handler = routes[key] ?? handle_missing
  let resp = handler(req)
  let [out, e2] = json.stringify(resp)
  bytes = bytes + len(out)
}
let t1 = time.monotonic()
print(`server_request: bytes=${bytes} elapsed_ms=${t1 - t0}`)
