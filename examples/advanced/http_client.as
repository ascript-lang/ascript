// http_client.as — exercises the modern std/net/http client against the
// companion server in http_server.as.
//
// Demonstrates: GET/POST, JSON request and response bodies, route params,
// 404 handling (a non-2xx response is NOT an error — `resp.ok` is false),
// bearer auth, per-request timeouts, and an automatic retry policy.
//
// Start the server first, then run this client:
//   ascript run examples/advanced/http_server.as   # terminal 1
//   ascript run examples/advanced/http_client.as   # terminal 2
//
// If the server isn't running, every call fails gracefully with a Tier-1
// error and the program still exits cleanly.

import { get, post } from "std/net/http"
import * as json from "std/json"

const BASE = "http://127.0.0.1:8787"

// GET a JSON endpoint and pretty-print the decoded payload.
async fn showJson(label, path, opts) {
  let [resp, err] = await get(`${BASE}${path}`, opts)
  if (err != nil) {
    print(`${label}: request failed — ${err.message}`)
    return
  }
  let [data, jerr] = await resp.json()
  if (jerr != nil) {
    print(`${label}: ${resp.status} but body was not JSON`)
    return
  }
  let okLabel = "not-ok"
  if (resp.ok) { okLabel = "OK" }
  let [s, _] = json.stringify(data)
  print(`${label}: ${resp.status} ${okLabel} -> ${s}`)
}

async fn main() {
  // 1. A healthy GET, with a generous total timeout.
  await showJson("health", "/health", { timeout: { total: 2000 } })

  // 2. A route parameter.
  await showJson("user 1", "/users/1", nil)

  // 3. A 404 — handled as data (resp.ok == false), not as an error.
  await showJson("missing", "/users/999", nil)

  // 4. A query string.
  await showJson("search", "/search?q=lovelace&page=2", nil)

  // 5. A POST with a text body; read the echoed response.
  let [resp, err] = await post(`${BASE}/echo`, { body: "hello from the client" })
  if (err == nil) {
    let [text, _] = await resp.text()
    print(`echo: ${resp.status} -> ${text}`)
  } else {
    print(`echo: failed — ${err.message}`)
  }

  // 6. A protected endpoint with bearer auth + a retry policy on 503s.
  await showJson("admin (authed)", "/admin/stats", {
    auth: { bearer: "s3cr3t" },
    retry: { max: 2, backoff: "exponential", baseDelay: 100, retryOn: [503] },
  })

  // 7. The same endpoint WITHOUT the token — expect 401.
  await showJson("admin (no token)", "/admin/stats", nil)
}

await main()
