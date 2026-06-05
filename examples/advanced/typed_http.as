// typed_http.as — resp.json(Class): decode an HTTP JSON body and validate it
// against a class in one step, fusing a decode failure and a shape mismatch into
// ONE Tier-1 [value, err] pair. With no class argument, resp.json() returns the
// raw decoded value unchanged.
//
// This example is SELF-CONTAINED: AScript is single-threaded, but async tasks let
// us run an in-process server (bound to an ephemeral port, stopped after a fixed
// number of requests via maxRequests) alongside the client in one program.
//
//   ascript run examples/advanced/typed_http.as

import { create } from "std/http/server"
import { get } from "std/net/http"
import * as json from "std/json"
import * as task from "std/task"
import * as schema from "std/schema"

const HOST = "127.0.0.1"

class User {
  id: number
  name: string
  role: string = "guest"
}

fn jsonResponse(status, value) {
  let [body, err] = json.stringify(value)
  if (err != nil) { return { status: 500, body: `serialize: ${err.message}` } }
  return { status: status, headers: { "content-type": "application/json" }, body: body }
}

let server = create()

// A well-shaped user.
server.route("GET", "/users/1", (req) => {
  return jsonResponse(200, { id: 1, name: "Ada Lovelace" })
})

// A wrong-shaped payload: id is a string, so validation must reject it.
server.route("GET", "/users/bad", (req) => {
  return jsonResponse(200, { id: "not-a-number", name: "Bug" })
})

// Typed path-param + query schemas (SP5 §2). Path params and query values arrive
// as STRINGS over HTTP; declaring a schema with coercion turns them into the right
// types BEFORE the handler runs. A bad value yields a 400 (the handler never runs).
server.get("/double/:n", { params: schema.object({ n: schema.number() }) }, (req) => {
  // req.params.n is a real number here, so arithmetic works.
  return jsonResponse(200, { doubled: req.params.n * 2 })
})
server.get("/search", { query: schema.object({ page: schema.number() }) }, (req) => {
  return jsonResponse(200, { page: req.query.page })
})

// Decode + validate a JSON user. `await resp.json(User)?` unwraps to a checked
// User instance, or propagates the fused [nil, err] on a failure.
async fn loadUser(base, path) {
  let [resp, rerr] = await get(`${base}${path}`)
  if (rerr != nil) { return Err(rerr) }
  let user = await resp.json(User)?
  return Ok(user)
}

// Wrap the native accept loop in a script `async fn` so calling it returns an
// eagerly-scheduled future that runs CONCURRENTLY with the client below. The
// server answers one request per connection (connection: close), so maxRequests
// counts the two connections the client opens, then the loop drains and returns.
async fn runServer() {
  await server.serve({ maxRequests: 4 })
}

async fn main() {
  // Bind to an ephemeral port so the example never collides with a real server.
  let [bound, berr] = await server.bind(HOST, 0)
  if (berr != nil) { print(`bind failed: ${berr.message}`); return }
  const base = `http://${HOST}:${bound}`

  // Serve exactly the two requests the client will make, then stop and drain.
  let serving = task.spawn(runServer())

  // 1. The good route: validated into a User instance.
  let [user, err1] = await loadUser(base, "/users/1")
  assert(err1 == nil, "good user has no error")
  assert(user.id == 1, "id validated")
  assert(user.name == "Ada Lovelace", "name validated")
  assert(user.role == "guest", "field default applied")
  print(`ok user: ${user.name} (role=${user.role})`)

  // 2. The wrong-shaped route: the shape mismatch fuses into the err channel
  //    (a non-number id), NOT a panic, NOT a raw object.
  let [bad, err2] = await loadUser(base, "/users/bad")
  assert(bad == nil, "bad shape -> nil value")
  assert(err2 != nil, "bad shape -> err")
  assert(err2.message != nil, "err has a message")
  print(`rejected bad payload: ${err2.message}`)

  // 3. Typed path param: "/double/21" coerces n="21" → 21, handler returns 42.
  let [dResp, dErr] = await get(`${base}/double/21`)
  assert(dErr == nil, "double request ok")
  let [dBody, _dbe] = await dResp.json()
  assert(dBody.doubled == 42, `param coerced + doubled: ${dBody.doubled}`)
  print(`typed param: doubled=${dBody.doubled}`)

  // 4. Typed query: "?page=3" coerces to a number.
  let [qResp, qErr] = await get(`${base}/search?page=3`)
  assert(qErr == nil, "query request ok")
  let [qBody, _qbe] = await qResp.json()
  assert(qBody.page == 3, `query coerced to number: ${qBody.page}`)
  print(`typed query: page=${qBody.page}`)

  // Let the server finish its accept loop and drain.
  await serving
}

await main()

print("typed_http ok")
