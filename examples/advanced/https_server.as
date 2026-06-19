// https_server.as — production-shaped HTTPS server using server.serve({tls}).
//
// Demonstrates TLS termination at the AScript layer (BATT A2): middleware,
// route parameters, a JSON API, and graceful shutdown via `process.on("SIGTERM")`.
//
// The certificate and private key are embedded as PEM string constants — no
// filesystem read at runtime — so the server works under `--deny fs` and
// keeps all TLS material in-process. See net.md for the PEM-vs-path rationale.
//
// This is a LONG-RUNNING server: it binds a port and blocks in the accept
// loop awaiting clients. Run it in one terminal, then probe with curl in
// another:
//
//   ascript run examples/advanced/https_server.as
//
//   curl --cacert src/stdlib/testdata/tls_test_cert.pem \
//        --resolve "localhost:8443:127.0.0.1" \
//        https://localhost:8443/health
//
//   curl --cacert src/stdlib/testdata/tls_test_cert.pem \
//        --resolve "localhost:8443:127.0.0.1" \
//        https://localhost:8443/users/1
//
//   curl -X POST \
//        --cacert src/stdlib/testdata/tls_test_cert.pem \
//        --resolve "localhost:8443:127.0.0.1" \
//        -H 'content-type: application/json' \
//        -d '{"name":"Grace"}' \
//        https://localhost:8443/echo
//
// Stop with Ctrl-C or send SIGTERM.

import { create, serve } from "std/http/server"
import * as json    from "std/json"
import * as object  from "std/object"
import * as process from "std/process"

// ── TLS credentials (self-signed localhost cert; valid until 2126) ─────────
//
// PEM strings are embedded here rather than read from the filesystem at startup
// so that:
//   a) No `fs` capability is required (only `net`).
//   b) The material stays within the process; no accidental path-traversal.
// In a production deployment, inject via environment variable or a secrets
// manager, then pass the PEM string directly to `tls.cert` / `tls.key`.
const CERT = "-----BEGIN CERTIFICATE-----
MIIDHzCCAgegAwIBAgIUCM49axUR3YM4Qj0lgC1rZq7krqYwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDYxOTExMjY0MFoYDzIxMjYw
NTI2MTEyNjQwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEB
AQUAA4IBDwAwggEKAoIBAQCnv4bR7w7UNp4vtjS2yinfWV085w3qS4CQc9RJc7fT
TJYehvHhyd2ZQl6LPULRjvxuNDDo0azX35sUaGn/S2EVTUcsPeBhFGToi2i74bdu
X0+gYyvJ40VQkKB049t34D0TmZ6S4H95hlN5E0yfpWWJR01sY6nah22bZGo2sxng
r1rytgyVl4oOmokiU/fyDZWos/Wj2ZJB98EcTELf16dJzs9ltx2Awz+v93YKPm+0
Po6IAq5iffwtNIsIKCGpyWjayqV2caN7VBt6cmlAp+CgZmdL26PbXAxmeCVByri1
17Rjw2+8lcZ7EiubjPxKyogYhR6cnHW4mMZh3r2n/cRZAgMBAAGjZzBlMAwGA1Ud
EwEB/wQCMAAwFAYDVR0RBA0wC4IJbG9jYWxob3N0MAsGA1UdDwQEAwIFoDATBgNV
HSUEDDAKBggrBgEFBQcDATAdBgNVHQ4EFgQUFnOG24LcCyVxfm216odz3l0qL9sw
DQYJKoZIhvcNAQELBQADggEBADnAygQa4Ged3Q+4ym1PSkSIihXaxuPkgLaFtMwo
0AXGRWXj6CrqYdxiz+bmG67I7XjXfIpzZsQQ1JJnkYwRmECbYjftAM89lOjO483h
HEF14UlOEA4U4LQY9B5YEwSzNwsMqPfPNWhueJzka8cfZxIlMUtdxLpzfLd3gXBv
UXQZCBvGXqdRdc5QHQ0koP0wASUBL603a2cCY8niLgaZMyE9CCfexlsTtz5NJSsl
5AQT23dv9OFEaSHqFA23fH3Hmfl4I8swA4X8MjLE7ZqfY1AmmJ3M21OOU9XL5zMe
GZJ72+AoghGeNGM4prn0hIQ5hC58PkPsHao/YjfkzHoOwvs=
-----END CERTIFICATE-----"

const KEY = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCnv4bR7w7UNp4v
tjS2yinfWV085w3qS4CQc9RJc7fTTJYehvHhyd2ZQl6LPULRjvxuNDDo0azX35sU
aGn/S2EVTUcsPeBhFGToi2i74bduX0+gYyvJ40VQkKB049t34D0TmZ6S4H95hlN5
E0yfpWWJR01sY6nah22bZGo2sxngr1rytgyVl4oOmokiU/fyDZWos/Wj2ZJB98Ec
TELf16dJzs9ltx2Awz+v93YKPm+0Po6IAq5iffwtNIsIKCGpyWjayqV2caN7VBt6
cmlAp+CgZmdL26PbXAxmeCVByri117Rjw2+8lcZ7EiubjPxKyogYhR6cnHW4mMZh
3r2n/cRZAgMBAAECggEAB6bsw7gqbo/JCp5bLFjE9DcVYDBJKf5BDRz3+sY76LEw
H+Wz+21unbg/yEY/WdWkLAHlCGDth/jDEEBepXYA0GWA8Pt9KelqseAPeJKaOQ2W
prsC8JJ5QzPLE1gu5TN0VctxRtGAMOuinyhm4qCQpPK1t+GaNNQ1lVTbh/HqQWTd
LQxfAL0TlE4Y0QLBl17nsYjwSvas7+VpWppXMw8mWEYCH6s42TlNjnmnDV2wtPUp
/G52Hdcma9foqKEA8NUHRwGmZKgmDGID0Rcuk8akOfdhXuf9J2d4hLq3pJSwLrkF
k3Sxghn6mZa7yqq0eWFwrx4+ajW0PdIzsgv8UEHT6QKBgQDkrusq4zqnfkKXsrFg
r9s3+2t/PSt1EUyGUdN+lsGl9U6sgWBJI5h1DR2w8AOrVP9X65dcsZW8ZclJAvBV
w8sjHaGRWpRRzO7TPBlflFdgkUQPAS1B6SHB2naWb1mFklDmc+jFSaDgF3j4hd94
78LR8H6rFa+X2TditMqtfZSLkwKBgQC7yTnf6HmhRT5xbUYnCE4wzm9VvH69Yttr
MzJWZ7ZNHaDLYp2wJrYruSAfk0SUdUZYNpMZrmatBEjAxBdBXFuLU2SO5ak2unlI
qaTlzBCjU0X8fUIyRd1b8QWAzO70nwSoN9tObow3n6xz/NI3yIM0oj8aaZdPjkJM
iMdm0qyb4wKBgHpbxWSrNGUOP59fc10iew9XLUtldW0sFmAAREOFcpPTz4apqtU3
gImQvQRBSBVSY1Wtrs1gD5hAdhTkx6d8HaLqZdqaNqYWGutXStRDUQVQdLP6kzaj
APbyZ2VSqvm3MiY8ep2lKbj9ljKTnuDcmMcwAPaVoeCDzwi3Z4KwoNyVAoGBAJjB
HMoOMvrD+AKOsFVKBUjgdGKa3cIzK2ftkpIE9Z+PbWBkzP8gzmmMwxvMUSoup9VU
N57ZZn5xkLj2CjDJ71HLuW4gVeDGGajJDvE7aYFiWPkF75YzjNignChlDDCDNmec
YFJRzM/mnIMRcvObsVdcb9aNdF9rynS1gvcagvyfAoGBAL4xc8K99YFOe0z7UB8S
pcJLpvNwchkixFA6P6wLOtWE8StWpA/NIC+R+wicY2vGEZWDPC2ociy0RmZl1C0U
tzwCkoyJlcKc4mnQB0FnL5+E3aJnfxPBDIY9wfs3X3OrCd2Okp+Sv/LLAPV0A2mq
VAiBLsOFc4kmFSvaxtJJXt9L
-----END PRIVATE KEY-----"

const HOST = "127.0.0.1"
const PORT = 8443

// ── In-memory "database" ───────────────────────────────────────────────────
let users = {
  "1": { id: 1, name: "Ada Lovelace",  role: "admin" },
  "2": { id: 2, name: "Alan Turing",   role: "user" },
  "3": { id: 3, name: "Grace Hopper",  role: "user" },
}
let requestCount = 0

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

// ── Build the app ──────────────────────────────────────────────────────────
let app = create()

// Middleware: log every request.
app.use((req, next) => {
  requestCount += 1
  print(`[${requestCount}] ${req.method} ${req.path}`)
  return next(req)
})

app.route("GET", "/health", (req) => {
  return jsonResponse(200, { status: "ok", users: len(object.keys(users)) })
})

app.route("GET", "/users/:id", (req) => {
  const user = users[req.params.id]
  if (user == nil) {
    return jsonResponse(404, { error: `no user ${req.params.id}` })
  }
  return jsonResponse(200, user)
})

app.route("POST", "/echo", (req) => {
  return jsonResponse(200, { youSent: req.body, length: len(req.body) })
})

// ── main ───────────────────────────────────────────────────────────────────
async fn main() {
  let [bound, berr] = await app.bind(HOST, PORT)
  if (berr != nil) {
    print(`could not bind ${HOST}:${PORT} — ${berr.message}`)
    return
  }
  print(`listening on https://${HOST}:${bound}  (Ctrl-C to stop)`)

  // Graceful shutdown on SIGTERM: register before serve() so the handler is
  // live while the accept loop runs. The signal fires after serve() returns,
  // but registering here keeps the program responsive even if serve() exits
  // for another reason.
  process.on("SIGTERM", (sig) => {
    print(`received ${sig}, shutting down`)
  })

  // serve({ tls }) — TLS is terminated at the AScript layer (rustls); the
  // {cert, key} PEM strings are shipped across the worker airlock so each
  // isolate builds its own rustls acceptor (Send-safe strings, not handles).
  let [_, serr] = await serve({
    port: bound,
    host: HOST,
    tls: { cert: CERT, key: KEY },
  })
  if (serr != nil) { print(`server error: ${serr.message}`) }
}

await main()
