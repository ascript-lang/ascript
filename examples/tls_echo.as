// tls_echo.as — self-contained TLS loopback round-trip demo.
//
// Demonstrates `tcp.connectTls` (BATT A1) and `server.serve({tls})` (BATT A2)
// in a single deterministic, self-terminating program — a corpus member for the
// four-mode differential gate.
//
// Pattern (mirrors examples/advanced/typed_http.as for the serve+client idiom):
//
//   1. Build the HTTP app, register one route.
//   2. `server.bind("127.0.0.1", 0)` → get the OS-assigned port.
//   3. `task.spawn(runServer())` — schedules the TLS serve CONCURRENTLY.
//   4. `tcp.connectTls("localhost", port, {caCert, serverName})` — TLS client.
//      caCert = the embedded self-signed cert (acts as its own CA root).
//      serverName = "localhost" matches the cert SAN.
//   5. Write a minimal HTTP/1.1 GET, read the status line, print it.
//   6. `await serving` — the server exits after maxRequests:1, unblocking this.
//
// The cert and key are embedded directly as PEM string constants (no filesystem
// read at runtime; no `fs` capability needed — see net.md §TLS for the rationale).
//
// Run:
//   cargo run --quiet -- run examples/tls_echo.as
//   cargo run --quiet -- run --tree-walker examples/tls_echo.as  (must match)

import * as tcp    from "std/net/tcp"
import * as server from "std/http/server"
import * as task   from "std/task"

// Self-signed cert for localhost (SAN = localhost, valid until 2126).
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

// ── 1. Build the server app: one route returns a fixed string ──────────────
let app = server.create()
app.route("GET", "/", (req) => "ok")

// ── 2. Bind on an OS-assigned port ────────────────────────────────────────
// bind() completes synchronously (no TLS at bind time); the port is known
// before the serve loop starts so the client can connect immediately.
let [port, berr] = await app.bind("127.0.0.1", 0)
if (berr != nil) {
  print("bind failed: " + berr.message)
  exit(1)
}

// ── 3. Schedule the TLS serve loop as a concurrent task ───────────────────
// task.spawn() eagerly schedules the future; maxRequests:1 causes the serve
// loop to exit after exactly one request, letting the program terminate.
// app.serve() runs the already-bound server (bind was called above). The TLS
// opts are passed here so the server wraps each accepted TCP connection in a
// rustls handshake before handing it to the HTTP layer.
async fn runServer() {
  let [_, serr] = await app.serve({ maxRequests: 1, tls: { cert: CERT, key: KEY } })
  if (serr != nil) { print("server error: " + serr.message) }
}
let serving = task.spawn(runServer())

// ── 4. Connect as a TLS client ────────────────────────────────────────────
// caCert: the same self-signed cert acts as its own CA root (no system CA
// needed; the gate fires hermetically without any external dependency).
// serverName: "localhost" matches the cert's Subject Alternative Name.
let [stream, cerr] = await tcp.connectTls("localhost", port, {
  caCert: CERT,
  serverName: "localhost",
})
if (cerr != nil) {
  print("connect failed: " + cerr.message)
  exit(1)
}

// ── 5. Write a minimal HTTP/1.1 GET and read the status line ──────────────
await stream.write("GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let statusLine = await stream.readLine()
print(statusLine)   // HTTP/1.1 200 OK
stream.close()

// ── 6. Wait for the server to drain and exit ──────────────────────────────
await serving
