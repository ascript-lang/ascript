// sessions.as — signed-cookie sessions end-to-end on a loopback server.
//
// The server signs a session into a tamper-evident cookie on login (HMAC-SHA256,
// verified constant-time) and reads it back on the next request via
// `server.session(req, secret)`. An ABSENT or TAMPERED session is rejected without
// trusting any unverified data.
//
// Deterministic + `maxRequests`-bounded → the example runs to completion (it is a
// four-mode corpus member, not a long-running server).

import { create } from "std/http/server"
import * as server from "std/http/server"
import { get, post } from "std/net/http"
import * as task from "std/task"

const HOST = "127.0.0.1"
const SECRET = "session-signing-key"

let app = create()

// POST /login — set a signed session cookie for "ada".
app.route("POST", "/login", (req) => {
  let signed = server.signCookie("session", { user: "ada", role: "admin" }, SECRET)
  let cookie = server.setCookie("session", signed, { path: "/", maxAge: 3600 })
  return {
    status: 200,
    headers: { "set-cookie": cookie },
    body: "logged-in",
  }
})

// GET /me — read the session back; 401 if absent/invalid.
app.route("GET", "/me", (req) => {
  let [sess, err] = server.session(req, SECRET)
  if (err != nil) {
    return { status: 401, body: "invalid session" }
  }
  if (sess.user == nil) {
    return { status: 401, body: "no session" }
  }
  return `hello ${sess.user} (${sess.role})`
})

async fn runServer() {
  // Three requests: /me (anon), /login, /me (authed).
  await app.serve({ maxRequests: 3 })
}

async fn main() {
  let [port, berr] = await app.bind(HOST, 0)
  if (berr != nil) {
    print(`bind failed: ${berr.message}`)
    return
  }
  let base = `http://${HOST}:${port}`
  let serving = task.spawn(runServer())

  // 1. /me with no session cookie → 401.
  let [r1, e1] = await get(`${base}/me`, nil)
  assert(e1 == nil, `GET /me (anon) failed: ${e1?.message}`)
  print(`anon /me: ${r1.status}`)

  // 2. login → capture the signed session cookie.
  let [r2, e2] = await post(`${base}/login`, { body: "" })
  assert(e2 == nil, `POST /login failed: ${e2?.message}`)
  let [body2, _] = await r2.text()
  print(`login: ${r2.status} ${body2}`)
  let sessionCookie = r2.cookies.session
  print(`got signed cookie: ${sessionCookie != nil}`)

  // 3. /me echoing the session cookie back → the decoded identity.
  let [r3, e3] = await get(`${base}/me`, {
    headers: { cookie: `session=${sessionCookie}` },
  })
  assert(e3 == nil, `GET /me (authed) failed: ${e3?.message}`)
  let [body3, __] = await r3.text()
  print(`authed /me: ${r3.status} ${body3}`)

  await serving
}

await main()
print("sessions ok")
