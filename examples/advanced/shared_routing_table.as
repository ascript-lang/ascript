// shared_routing_table.as — the zero-copy shared-heap fan-out pattern.
//
// Build a large read-only routing table ONCE on the main isolate, `shared.freeze`
// it, then hand the SAME frozen graph to every worker isolate. Each `worker fn`
// call receives the table as an Arc pointer bump (NOT a per-call structured-clone
// of the whole table) and reads it zero-copy. This is the production shape for a
// per-request shared snapshot (a routing table, a feature-flag set, a geo-IP DB):
// freeze once, read across cores, pay zero per-dispatch copy.
//
// Fully error-handled: a non-freezable value would be a recoverable Tier-2 panic at
// freeze time, and gather preserves input order so the output is deterministic
// regardless of which isolate answers first.
//
//   ascript run examples/advanced/shared_routing_table.as
import * as shared from "std/shared"
import * as task from "std/task"
import * as array from "std/array"

// Build a routing table: method+path -> handler name. In a real service this could
// be megabytes (compiled routes, a geo database). Here it is small but the pattern
// is identical — the table crosses to each isolate by pointer, not by copy.
fn buildRoutes() {
  return {"GET /": "home", "GET /health": "health", "GET /users": "listUsers", "GET /users/:id": "getUser", "POST /users": "createUser", "DELETE /users/:id": "deleteUser", "GET /orders/:id": "getOrder", "POST /orders": "createOrder"}
}

// Freeze once. After this, every read and every cross-isolate hand-off is O(1).
let routes = shared.freeze(buildRoutes())

// A worker that resolves one request key against the shared table. The table arrives
// as an Arc bump; the lookup is a zero-copy read of the frozen object.
worker fn resolve(table, key) {
  let handler = table[key]
  if (handler == nil) {
    return `404 ${key}`
  }
  return `${key} -> ${handler}`
}

async fn main() {
  // A batch of incoming request keys (some hit; "GET /nope" misses -> 404).
  let requests = ["GET /", "GET /users", "GET /users/:id", "POST /orders", "GET /nope", "DELETE /users/:id"]

  // Fan out: one worker call per request, each reading the SAME frozen table.
  let futures = array.map(requests, (key) => resolve(routes, key))
  let resolved = await task.gather(futures) // order preserved
  for (line of resolved) {
    print(line)
  }

  // The table is immutable even on the main isolate.
  let [_, err] = recover(() => routes["GET /"] = "hijacked")
  print(err.message) // cannot mutate a frozen object
}

await main()
