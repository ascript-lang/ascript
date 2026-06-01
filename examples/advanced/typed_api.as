import { create } from "std/http/server"
import { get, post } from "std/net/http"
import * as schema from "std/schema"
import * as json from "std/json"
import * as task from "std/task"
const HOST = "127.0.0.1"
let nextId = 1
let users = []
fn findUser(id) {
  for (u of users) {
    if (`${u.id}` == id) {
      return u
    }
  }
  return nil
}
fn jsonResp(status, value) {
  let [body, err] = json.stringify(value)
  if (err != nil) {
    return { status: 500, body: `serialize error: ${err.message}` }
  }
  return { status: status, headers: { "content-type": "application/json" }, body: body }
}
let server = create()
server.use((req, next) => {
  print(`  >> ${req.method} ${req.path}`)
  return next(req)
})
server.get("/users/:id", req => {
  const user = findUser(req.params.id)
  if (user == nil) {
    return jsonResp(404, { error: `no user with id ${req.params.id}` })
  }
  return jsonResp(200, user)
})
const userSchema = schema.object({ name: schema.string(), age: schema.number() })
server.post("/users", userSchema, req => {
  const u = { id: nextId, name: req.body.name, age: req.body.age }
  nextId = nextId + 1
  users = [...users, u]
  return jsonResp(201, u)
})
server.get("/ping", req => {
  return jsonResp(200, { pong: true, total: len(users) })
})
async fn runServer() {
  await server.serve({ maxRequests: 6 })
}
fn makeJsonBody(value) {
  let [s, err] = json.stringify(value)
  if (err != nil) {
    return ""
  }
  return s
}
async fn main() {
  let [port, berr] = await server.bind(HOST, 0)
  if (berr != nil) {
    print(`bind failed: ${berr.message}`)
    return
  }
  const base = `http://${HOST}:${port}`
  print(`server bound on ${base}`)
  let serving = task.spawn(runServer())
  let [r1, e1] = await post(`${base}/users`, { body: makeJsonBody({ name: "Ada Lovelace", age: 30 }), headers: { "content-type": "application/json" } })
  assert(e1 == nil, `POST /users (valid) failed: ${e1?.message}`)
  let [body1, jerr1] = await r1.json()
  assert(jerr1 == nil, "POST /users response is valid JSON")
  assert(body1.id == 1, "first user gets id=1")
  assert(body1.name == "Ada Lovelace", "name preserved")
  print(`created user: ${body1.name} (id=${body1.id})`)
  let [r2, e2] = await post(`${base}/users`, { body: makeJsonBody({ name: "Alan Turing", age: 41 }), headers: { "content-type": "application/json" } })
  assert(e2 == nil, `POST /users (valid #2) failed: ${e2?.message}`)
  let [body2, jerr2] = await r2.json()
  assert(jerr2 == nil, "POST /users #2 response is valid JSON")
  assert(body2.id == 2, "second user gets id=2")
  print(`created user: ${body2.name} (id=${body2.id})`)
  let [r3, e3] = await get(`${base}/users/1`)
  assert(e3 == nil, "GET /users/1 should not error")
  let [body3, jerr3] = await r3.json()
  assert(jerr3 == nil, "GET /users/1 response is valid JSON")
  assert(body3.name == "Ada Lovelace", "GET returns the correct user")
  print(`fetched user: ${body3.name}`)
  let [r4, e4] = await get(`${base}/ping`)
  assert(e4 == nil, "GET /ping should not error")
  let [pong, jerr4] = await r4.json()
  assert(jerr4 == nil, "GET /ping response is valid JSON")
  assert(pong.pong == true, "ping returns pong:true")
  assert(pong.total == 2, "total reflects two created users")
  print(`ping: total=${pong.total}`)
  let [r5, e5] = await post(`${base}/users`, { body: "not json" })
  assert(e5 == nil, "HTTP round-trip itself should not error")
  assert(r5.status == 400, `malformed JSON body should yield 400, got ${r5.status}`)
  print(`malformed JSON correctly rejected with 400`)
  let [r6, e6] = await post(`${base}/users`, { body: makeJsonBody({ name: "X", age: "old" }), headers: { "content-type": "application/json" } })
  assert(e6 == nil, "HTTP round-trip itself should not error")
  assert(r6.status == 400, `schema mismatch should yield 400, got ${r6.status}`)
  print(`schema mismatch correctly rejected with 400`)
  await serving
}
await main()
print("typed_api ok")
