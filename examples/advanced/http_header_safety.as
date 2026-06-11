// http_header_safety.as — response header validation (HTTP response splitting guard)
//
// AScript's HTTP server validates every handler-supplied response header name and
// value BEFORE writing it to the wire. A handler that reflects untrusted input into
// a header value containing CR/LF (or an invalid header name) cannot inject extra
// headers or a whole second response — the malformed header is rejected and the
// request fails closed with a 500 instead of being SPLIT.
//
// This example is SELF-CONTAINED: an in-process server (bound to an ephemeral port,
// stopped after a fixed number of requests via maxRequests) runs concurrently with
// a client in one single-threaded program.
//
//   ascript run examples/advanced/http_header_safety.as

import { create } from "std/http/server"
import { get } from "std/net/http"
import * as task from "std/task"

const HOST = "127.0.0.1"

let server = create()

// A WELL-BEHAVED handler: a normal header (alphanumerics + `-`, a clean value).
// This passes validation and reaches the client verbatim.
server.route("GET", "/safe", (req) => {
  return { status: 200, headers: { "X-Request-Id": "abc-123" }, body: "ok" }
})

// A VULNERABLE-LOOKING handler: it reflects attacker-controlled input straight into
// a response header value. The input contains CRLF + a forged header line — the
// classic response-splitting payload. The server REJECTS it (→ 500); the forged
// `X-Injected` header never reaches the client.
server.route("GET", "/reflect", (req) => {
  // Imagine `tainted` came from a query param or request header.
  let tainted = "valid\r\nX-Injected: pwned"
  return { status: 200, headers: { "X-Echo": tainted }, body: "ok" }
})

async fn runServer() {
  // Two client connections below; connection: close → one request each.
  await server.serve({ maxRequests: 2 })
}

async fn main() {
  let [bound, berr] = await server.bind(HOST, 0)
  if (berr != nil) { print(`bind failed: ${berr.message}`); return }
  const base = `http://${HOST}:${bound}`

  let serving = task.spawn(runServer())

  // 1. The safe route: the legitimate header survives validation.
  let [okResp, okErr] = await get(`${base}/safe`)
  assert(okErr == nil, "safe request ok")
  assert(okResp.status == 200, "safe route returns 200")
  assert(okResp.headers["x-request-id"] == "abc-123", "valid header passed through")
  print(`safe header preserved: X-Request-Id=${okResp.headers["x-request-id"]}`)

  // 2. The reflecting route: the CRLF-bearing header is rejected, so the response
  //    is a clean 500 — NOT a split 200 — and no injected header is present.
  let [badResp, badErr] = await get(`${base}/reflect`)
  assert(badErr == nil, "reflect request completes (a single, un-split response)")
  assert(badResp.status == 500, "CRLF header rejected -> 500, not a split 200")
  assert(badResp.headers["x-injected"] == nil, "no injected header reached the client")
  print(`response-splitting attempt rejected: status=${badResp.status}, X-Injected=${badResp.headers["x-injected"]}`)

  await serving
}

await main()

print("http_header_safety ok")
