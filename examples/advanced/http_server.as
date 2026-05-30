// http_server.as — a small JSON API built on std/http/server.
//
// Demonstrates: middleware (request logging + a bearer-auth gate), route
// parameters (`/users/:id`), query strings, reading a request body, returning
// structured `{status, headers, body}` responses, and graceful 404/400 handling.
//
// AScript is single-threaded, so a full request/response round-trip needs the
// client in a SEPARATE process. Run this server, then in another terminal run
// the companion client:
//
//   ascript run examples/advanced/http_server.as      # terminal 1 (blocks, serving)
//   ascript run examples/advanced/http_client.as      # terminal 2
//
// Or probe it with curl:
//   curl localhost:8787/health
//   curl localhost:8787/users/1
//   curl -X POST localhost:8787/echo -d 'hello'
//   curl -H 'authorization: Bearer s3cr3t' localhost:8787/admin/stats

import { create } from "std/http/server"
import * as json from "std/json"
import * as string from "std/string"
import * as object from "std/object"

const HOST = "127.0.0.1"
const PORT = 8787
const ADMIN_TOKEN = "s3cr3t"

// A tiny in-memory "database" keyed by string id (route params are strings).
let users = {
  "1": { id: 1, name: "Ada Lovelace",   role: "admin" },
  "2": { id: 2, name: "Alan Turing",    role: "user" },
  "3": { id: 3, name: "Grace Hopper",   role: "user" },
}
let requestCount = 0

// Build a JSON response object the server understands: { status, headers, body }.
fn jsonResponse(status, value) {
  let [body, err] = json.stringify(value, true)
  if (err != nil) {
    return { status: 500, body: `serialization error: ${err.message}` }
  }
  return {
    status: status,
    headers: { "content-type": "application/json" },
    body: body,
  }
}

let server = create()

// --- Middleware 1: log every request and count it. -----------------------
server.use((req, next) => {
  requestCount += 1
  print(`[${requestCount}] ${req.method} ${req.path}`)
  return next(req)
})

// --- Middleware 2: guard the /admin/* prefix with a bearer token. ---------
server.use((req, next) => {
  if (string.find(req.path, "/admin") == 0) {
    const auth = req.headers["authorization"] ?? ""
    if (auth != `Bearer ${ADMIN_TOKEN}`) {
      return jsonResponse(401, { error: "unauthorized" })
    }
  }
  return next(req)
})

// --- Routes ---------------------------------------------------------------

server.route("GET", "/health", (req) => {
  return jsonResponse(200, { status: "ok", users: len(object.keys(users)) })
})

server.route("GET", "/users/:id", (req) => {
  const user = users[req.params.id]
  if (user == nil) {
    return jsonResponse(404, { error: `no user ${req.params.id}` })
  }
  return jsonResponse(200, user)
})

server.route("POST", "/echo", (req) => {
  return jsonResponse(200, { youSent: req.body, length: len(req.body) })
})

server.route("GET", "/search", (req) => {
  const q = req.query.q ?? ""
  const page = req.query.page ?? "1"
  return jsonResponse(200, { query: q, page: page })
})

server.route("GET", "/admin/stats", (req) => {
  return jsonResponse(200, { totalRequests: requestCount, userCount: len(object.keys(users)) })
})

async fn main() {
  let [bound, err] = await server.bind(HOST, PORT)
  if (err != nil) {
    print(`could not bind ${HOST}:${PORT} — ${err.message}`)
    return
  }
  print(`listening on http://${HOST}:${bound}  (Ctrl-C to stop)`)
  // serve() loops forever; pass { maxRequests: N } to stop after N requests.
  let [_, serr] = await server.serve()
  if (serr != nil) { print(`server error: ${serr.message}`) }
}

await main()
