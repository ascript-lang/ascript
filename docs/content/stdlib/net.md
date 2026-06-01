:::eyebrow Standard library

# Networking & HTTP

AScript's networking stack — DNS resolution, raw TCP, UDP datagrams, a modern HTTP client, a small HTTP server, and WebSockets — lives in several modules under `std/net`. All are provided by the `net` Cargo feature, which is **enabled by default**. If you build AScript with a custom feature set, include `net` to keep these modules available.

The interpreter is **single-threaded with an inline async model**: there is no background thread of execution, and almost every networking operation suspends the program until it completes. Consequently nearly every method on these modules is `await`ed — `lookup`, `connect`, `accept`, `read`, `write`, `send`, `recv`, the HTTP verbs, `serve`, and so on. The synchronous exceptions are the handle-teardown methods (`close()`) and the in-memory builders on the server handle (`route`, `use`).

> [!NOTE] AScript has no task-spawn primitive. A self-contained TCP loopback works because `connect()` completes into the OS listen backlog before `accept()`. A full HTTP/WebSocket round-trip needs the server and client in **separate processes** (the handshake needs the server's accept loop running). See the examples page.

Throughout these modules, fallible operations follow the **Tier-1** convention: they return a two-element `[value, err]` pair where `err` is `nil` on success. Misuse (wrong argument *types*, malformed options) is a **Tier-2** panic.

> [!TIER1] Destructure every fallible call: `let [ips, err] = await net.lookup(host)`. A connect/DNS/TLS/timeout failure surfaces as `[nil, err]`; success as `[value, nil]`.

## std/net

General networking utilities: DNS resolution.

```ascript
import * as net from "std/net"
```

### net.lookup

Resolves a hostname to a de-duplicated list of IP-address strings. Async; returns `[array<string>, err]`.

- `host` (string) — a hostname (e.g. `"localhost"`, `"example.com"`) or a `"host:port"` pair. A bare hostname without a port has `:0` appended internally before resolution. The returned strings contain only the IP address (port stripped).
- Returns: `[ips, err]` — `ips` is an `array<string>` of resolved IPs in first-seen order, de-duplicated. On failure, `[nil, err]`.

```ascript
import * as net from "std/net"

let [ips, err] = await net.lookup("localhost")
if (err != nil) { print("DNS failed: " + err.message) }
print(ips)   // e.g. ["127.0.0.1", "::1"]
```

### net.lookupOne

Resolves a hostname and returns only the first IP address. Async; returns `[string, err]`.

- `host` (string) — same form as `net.lookup`.
- Returns: `[ip, err]` — the first resolved IP as a string, or `[nil, err]` if resolution fails or returns zero addresses.

```ascript
import * as net from "std/net"

let [ip, err] = await net.lookupOne("example.com")
if (err != nil) { print("DNS failed: " + err.message) }
print(ip)   // e.g. "93.184.216.34"
```

## std/net/udp

UDP datagram sockets. Bind an ephemeral port, send datagrams to any peer, and receive from any sender.

```ascript
import * as udp from "std/net/udp"
```

### udp.bind

Binds a UDP socket to a local address. Returns `[socket, err]`.

- `addr` (string) — a `"host:port"` string. Use port `0` for an OS-assigned ephemeral port; read the actual port back with `socket.localAddr()`.
- Returns: `[socket, err]`.

```ascript
import * as udp from "std/net/udp"

let [sock, err] = udp.bind("127.0.0.1:0")
if (err != nil) { print(err.message) }
print(sock.localAddr())   // e.g. "127.0.0.1:54321"
```

### Socket methods

A bound socket handle exposes:

- `await socket.send(data, addr)` — sends `data` (a string or bytes) to the peer at `"host:port"`. Returns `[bytesSent, err]`. Async.
- `await socket.recv()` — waits for and returns the next incoming datagram as `[{data, from}, err]`. `data` is bytes (use `std/encoding`'s `utf8Decode` to decode as text); `from` is the sender's `"ip:port"` string. Async. Buffer cap: 65 507 bytes (max UDP payload over IPv4).
- `socket.localAddr()` — returns the bound `"ip:port"` string. Synchronous.
- `socket.close()` — releases the socket. Synchronous; idempotent. After `close()`, `send` and `recv` return Tier-1 errors rather than panicking.

### UDP loopback echo

Because `send()` deposits the datagram directly into the OS kernel queue before `recv()` runs, a single-process UDP echo works without deadlocking — the same guarantee that makes the TCP loopback example work:

```ascript
import * as udp from "std/net/udp"
import * as encoding from "std/encoding"

let [sockA, _eA] = udp.bind("127.0.0.1:0")
let [sockB, _eB] = udp.bind("127.0.0.1:0")

let addrB = sockB.localAddr()

// send() deposits the datagram; no deadlock even without a concurrent receiver.
let [sent, sendErr] = await sockA.send("hello udp", addrB)
print(sendErr)    // nil
print(sent)       // 9

let [pkt, recvErr] = await sockB.recv()
print(recvErr)    // nil
let [text, _] = encoding.utf8Decode(pkt.data)
print(text)       // hello udp
print(pkt.from)   // 127.0.0.1:<port>

sockA.close()
sockB.close()
```

## std/net/tcp

Raw TCP client and listener handles, built on tokio so they ride the event loop. Two module entry points open connections; the returned handles carry the read/write methods.

```ascript
import * as tcp from "std/net/tcp"
```

A stream is **bytes-oriented**: `read`/`readToEnd` return bytes, while `readLine` decodes a UTF-8-lossy line for line protocols. A stream **finalizes itself on EOF** — a read after end-of-stream returns `nil` (or empty bytes for `readToEnd`) rather than panicking, and the socket fd drops promptly.

### tcp.connect

Opens a client TCP connection. Async; returns `[stream, err]`.

- `host` (string) — the host to connect to.
- `port` (number) — an integer in `0..=65535`.
- Returns: `[stream, err]` — a connect failure yields `[nil, err]`.

```ascript
let [stream, err] = await tcp.connect("127.0.0.1", 8080)
if (err != nil) { /* handle */ }
```

### tcp.listen

Binds a TCP listener. Async; returns `[listener, err]`.

- `host` (string) — the host/interface to bind.
- `port` (number) — an integer in `0..=65535`. **Port `0` means OS-assigned** — read the real port off `listener.port`.
- Returns: `[listener, err]`.

```ascript
let [server, err] = tcp.listen("127.0.0.1", 0)
let port = server.port   // the OS-assigned port
```

### TCP stream methods

A connected stream (from `connect` or `accept`) exposes:

- `await stream.read(n?)` — reads up to `n` bytes (default 64 KiB if omitted). Returns **bytes**, or **`nil` at EOF**. `read(0)` returns empty bytes without touching the socket. A negative `n` is a Tier-2 panic.
- `await stream.readLine()` — reads a single line, stripping a trailing `\n` (and an optional preceding `\r`). Returns a **string**, or **`nil` at EOF**.
- `await stream.readToEnd()` — reads to end-of-stream. Always returns **bytes** (empty if already drained); consumes and finalizes the stream.
- `await stream.write(data)` — writes a string or bytes. Returns `[nil, err]` — a write to a closed stream returns `[nil, err]` rather than panicking.
- `stream.close()` — synchronous; drops the socket. Idempotent.

### TCP listener

A bound listener exposes:

- `listener.port` — the bound port number (the OS-assigned one when you bound port `0`).
- `await listener.accept()` — accepts the next connection. Returns `[stream, err]`; the accepted `stream` has the same methods as a connected one.
- `listener.close()` — synchronous; stops accepting.

### Self-contained loopback echo

Because `connect()` completes the TCP handshake into the OS listen backlog **before** any matching `accept()` runs, a pure-loopback round-trip works in a single process without deadlocking:

```ascript
import * as tcp from "std/net/tcp"

// Bind a listener on an ephemeral port (port 0 -> OS picks a free one).
let [server, e1] = tcp.listen("127.0.0.1", 0)
print(e1)
let port = server.port

// connect() completes into the listen backlog before we accept().
let [client, e2] = await tcp.connect("127.0.0.1", port)
print(e2)

// accept() dequeues the queued connection — no deadlock, single-threaded.
let [conn, e3] = await server.accept()
print(e3)

// Round-trip a line: client -> server.
await client.write("ping\n")
let line = await conn.readLine()
print(line) // ping

// Echo it back: server -> client.
await conn.write("pong\n")
let reply = await client.readLine()
print(reply) // pong

client.close()
conn.close()
server.close()
```

## std/net/http

A modern HTTP client built on reqwest. It offers the seven HTTP verbs plus a generic `request`, a cancellation primitive, and a first-class Server-Sent-Events client.

```ascript
import * as http from "std/net/http"
```

### The verbs and request

Each verb takes a URL and an optional options object, is async, and returns `[resp, err]`:

- `await http.get(url, opts?)`
- `await http.post(url, opts?)`
- `await http.put(url, opts?)`
- `await http.patch(url, opts?)`
- `await http.delete(url, opts?)`
- `await http.head(url, opts?)`
- `await http.options(url, opts?)`
- `await http.request(opts)` — the generic form; `opts.method` selects the verb (default `GET`) and `opts.url` is **required**.

A connect/DNS/TLS/timeout failure (or a cancellation, or a total-timeout expiry) is the Tier-1 `[nil, err]`.

> [!NOTE] A non-2xx response is **not** an error. It is a normal `resp` with `ok == false`. To turn a non-2xx status into a Tier-1 error instead, set `errorOnStatus: true` in the options.

### The response object

On success, `resp` carries these metadata fields, read eagerly before the body is touched:

| field | type | meaning |
| --- | --- | --- |
| `status` | number | the HTTP status code (e.g. `200`, `404`). |
| `ok` | bool | `true` for a 2xx status. |
| `version` | string | the negotiated HTTP version: `"1.1"`, `"2"`, etc. |
| `url` | string | the final URL (after any redirects). |
| `headers` | object | response headers, **keys lowercased**; last value wins on repeats. |
| `cookies` | object | `name → value` parsed from `Set-Cookie` (attributes after the first `;` are dropped). |
| `trailers` | object | always an empty object — reqwest's high-level API does not surface HTTP trailing headers. |
| `body` | reader | **present only in streaming mode** (`opts.stream: true`) — see below. |

### Reading the body

Three async accessor methods read the (buffered) body. Each returns `[value, err]`:

- `await resp.text()` → `[string, err]`
- `await resp.bytes()` → `[bytes, err]`
- `await resp.json()` → `[value, err]` — parses the body as JSON into an AScript value.
- `await resp.json(Class)` → `[instance, err]` — parses **and validates** the body against a class in one step.

> [!TIER2] Each accessor **consumes** the response. Calling a second body accessor on the same response is a Tier-2 panic: `"response body already consumed"`. On a streaming response, these accessors are unavailable — read `resp.body` instead.

```ascript
let [resp, err] = await http.get("https://example.com/data.json")
if (err != nil) { /* network error */ }
if (!resp.ok) { /* status >= 300 — still a valid resp */ }
let [data, jerr] = await resp.json()
```

#### Typed parse: `resp.json(Class)`

Passing a [class](../language/classes-enums) as an argument fuses JSON decoding and shape validation
into a single Tier-1 result. A decode failure **and** a shape mismatch both surface as `[nil, err]`
in the *same* error channel — neither panics. The class is an ordinary value argument (no generics);
on success the value is a validated instance (defaults applied, optionals defaulted to nil), exactly
as if you had called [`Class.from`](../language/classes-enums) on the decoded object.

An optional trailing `strict` bool — `await resp.json(User, true)?` — rejects any key not declared
on the class (at every nesting level), fused into the same `err` channel. Omitted or `false`,
unknown keys are ignored (lenient, the default).

```ascript
class User {
  id: number
  name: string
  role: string = "guest"
}

// `?` and `!` bind looser than `await`, so no parens are needed:
let user = await resp.json(User)?     // unwrap to a User, or propagate [nil, err]

// Or handle the fused error explicitly:
let [u, err] = await resp.json(User)
// err != nil on bad JSON OR a wrong shape (e.g. a non-number id)
```

See [typed parse on the data page](data) for the standalone `json.parse(text, Class)` form, and the
runnable `examples/advanced/typed_http.as` for an end-to-end client+server demo.

### Request options

Every verb (and `request`) accepts an options object. All keys are optional.

| key | shape | meaning |
| --- | --- | --- |
| `query` | object | merged onto the URL as `?k=v`; an array value expands to repeated keys (`k=a&k=b`). |
| `headers` | object (string→scalar) | request headers. |
| `auth` | `{bearer: tok}` or `{basic: [user, pass?]}` | sets the `Authorization` header. |
| `body` | string · bytes · object | request body — see **Body shapes** below. |
| `timeout` | `{connect?, read?, total?}` (ms) | `connect` is independent; `read` folds into the total timeout when `total` is unset. A total-timeout expiry is a Tier-1 error. |
| `redirect` | `{follow?, max?}` or `"none"` | default: follow, max 10. `"none"` (or `follow:false`) disables redirects. |
| `retry` | `{max, backoff?, baseDelay?, retryOn?}` | OFF by default — see **Retries** below. |
| `decompress` | bool | default `true`. `false` disables all transparent decoders. |
| `tls` | `{caBundle?, clientCert?, minVersion?, sni?, insecure?}` | TLS configuration. `insecure: true` disables cert verification (testing only). |
| `cookies` | bool | `true` enables a per-request cookie jar (persists across redirects within that request). |
| `proxy` | string | `"http://…"` / `"https://…"` / `"socks5://…"` / `"system"` / `"none"`. |
| `httpVersion` | string | `"auto"` (default), `"1.1"`, `"2"`, or `"3"`. `"3"` requires the `http3` build feature; otherwise a clean Tier-1 error. |
| `errorOnStatus` | bool | `true` turns a non-2xx response into a Tier-1 error. |
| `cancel` | cancelToken handle | abort the in-flight send — see **Cancellation** below. |
| `stream` | bool | `true` exposes `resp.body` as a streaming reader instead of buffering. |
| `bodyMode` | string | for streaming bodies: `"string"` (default) or `"bytes"`. |

#### Body shapes

`opts.body` accepts:

- **a string** or **bytes** — sent verbatim.
- `{json: value}` — serialized to JSON and sent with `Content-Type: application/json`.
- `{form: object}` — URL-encoded and sent as `application/x-www-form-urlencoded`. Array values expand to repeated keys.
- `{multipart: [...]}` — a multipart form. Each part is `{name, value}` for a text field, or `{name, data, filename?, contentType?}` for a file/bytes part (`data` is a string or bytes).
- `{stream: source}` — a streamed request body. A `bytes` source streams verbatim; a **reader handle** (a `std/process` Reader, a TCP stream, or an HTTP body) or an **async-generator function** `() => [bytes, err]` is drained into a buffer and then sent.

#### Retries

`opts.retry` is `{max, backoff, baseDelay, retryOn}`. With `max > 0`, the request is retried on connection errors (for idempotent methods: GET/HEAD/PUT/DELETE/OPTIONS) and on any response whose status is in `retryOn`. `backoff` is `"exponential"` (default, `baseDelay * 2^attempt`) or `"constant"`; `baseDelay` defaults to 100 ms. Non-cloneable (streaming) bodies cannot be retried.

#### Cancellation

`http.cancelToken()` returns a cancel-token handle. Pass it as `opts.cancel`; calling `token.cancel()` aborts the in-flight send, which then resolves to a Tier-1 `[nil, err]`.

```ascript
let token = http.cancelToken()
// ... elsewhere: token.cancel()
let [resp, err] = await http.get(url, { cancel: token })
```

### Example: JSON POST with auth and retry

```ascript
import * as http from "std/net/http"

let [resp, err] = await http.post("https://api.example.com/items", {
  auth: { bearer: "my-token" },
  body: { json: { name: "widget", qty: 3 } },
  retry: { max: 3, backoff: "exponential", baseDelay: 200, retryOn: [502, 503] },
  timeout: { connect: 2000, total: 10000 },
})
if (err != nil) { print("request failed: " + err.message) }
print(resp.status)
let [created, jerr] = await resp.json()
```

### Example: multipart upload

```ascript
import * as http from "std/net/http"
import * as fs from "std/fs"

let [bytes, _e] = fs.readBytes("avatar.png")
let [resp, err] = await http.post("https://api.example.com/upload", {
  body: { multipart: [
    { name: "title", value: "My avatar" },
    { name: "file", data: bytes, filename: "avatar.png", contentType: "image/png" },
  ] },
})
```

### Streaming responses

With `opts.stream: true`, the body is not buffered: `resp.body` is a reader handle that pulls chunks on demand (a slow consumer applies backpressure to the transfer). It supports the same reader idiom as a TCP stream:

- `await resp.body.read(n?)` → a chunk (string or bytes per `opts.bodyMode`), or `nil` at EOF.
- `await resp.body.readLine()` → a line, or `nil` at EOF.
- `await resp.body.readToEnd()` → the remainder (always in the body's mode).

The body finalizes itself on EOF. The buffered `text()`/`bytes()`/`json()` accessors are **not** available on a streaming response.

```ascript
import * as http from "std/net/http"

let [resp, err] = await http.get("https://example.com/big.log", { stream: true })
loop {
  let line = await resp.body.readLine()
  if (line == nil) { break }
  print(line)
}
```

### Server-Sent Events

`http.sse(url, opts?)` opens a first-class SSE client: it GETs with `Accept: text/event-stream` and returns `[stream, err]`. The stream parses the SSE wire format.

- `await stream.next()` → `[event, err]`, or `nil` when the stream ends. Each `event` is an object `{event, data, id, retry}` — `event` defaults to `"message"`; `data` joins multi-line `data:` fields with `\n`; `id` and `retry` are the most recent values or `nil`.
- `stream.lastEventId` — a live field holding the most recent `id:`.
- `stream.close()` — ends the stream.

SSE accepts `opts.headers` and `opts.auth` (same shapes as the verbs). **Auto-reconnect is ON by default**: on disconnect the stream waits the server-provided `retry:` interval (or `opts.retryDefault`, default 3000 ms), reconnects with the `Last-Event-ID` header, and resumes. `opts.reconnect: false` disables this; `opts.maxReconnects` caps the attempts.

```ascript
import * as http from "std/net/http"

let [stream, err] = await http.sse("https://example.com/events", {
  auth: { bearer: "my-token" },
})
if (err != nil) { print(err.message) }
loop {
  let [event, eerr] = await stream.next()
  if (event == nil) { break }   // stream ended
  print(event.event + ": " + event.data)
}
stream.close()
```

## std/http/server

A minimal HTTP/1 server whose request handlers are AScript functions. Because handlers need `&mut Interp` on the single-threaded runtime, requests are handled **strictly sequentially** (one request per connection, fully awaited before the next). Concurrent connections are a documented v1 limitation.

```ascript
import { create } from "std/http/server"
```

### create

`create()` returns a server handle. There are no arguments and it is not async.

```ascript
let server = create()
```

### Server methods

- `server.route(method, path, handler)` — registers a route. `method` is matched case-insensitively. `path` may contain `:name` segments captured into `req.params`. `handler` is `(req) => response`. Returns the server (chainable). Synchronous.
- `server.use(middleware)` — registers `(req, next) => response` middleware, run in registration order before the route. A middleware may short-circuit by returning a response without calling `next`, or call `next(req?)` to advance the chain (optionally replacing the request). `next` is single-use. Returns the server. Synchronous.
- `await server.bind(host, port)` → `[boundPort, err]` — binds a listener **without** looping. Bind port `0` and read the OS-assigned `boundPort`.
- `await server.serve(opts?)` → `[nil, err]` — runs the accept loop. `opts` may set `maxRequests` (return after serving N requests — useful for tests/shutdown), `maxBodySize` (default 16 MiB), and `requestTimeout` (ms, default 30000). With no `maxRequests`, it loops until the listener errors.
- `await server.listen(host, port, opts?)` → `bind` + `serve` for the common case.
- `server.close()` — synchronous; drops the server.

#### Verb shorthand methods

Each of the seven standard verbs has a named shorthand — sugar over `server.route(METHOD, path, handler)`:

```ascript
server.get(path, handler)
server.post(path, handler)
server.put(path, handler)
server.patch(path, handler)
server.delete(path, handler)
server.head(path, handler)
server.options(path, handler)
```

All are synchronous, chainable, and accept the same `path`/`handler` form as `route`. HEAD responses automatically suppress the body bytes while preserving the `Content-Length` header (RFC 9110 §9.3.2).

#### Schema-validated routes

A three-argument verb call (or four-argument `route`) attaches a [std/schema](schema) validator to the route. When a request arrives, the body is JSON-decoded and validated **before** the handler runs:

```ascript
import * as schema from "std/schema"

// 3-arg verb form:
server.post(path, schema, handler)
server.put(path, schema, handler)

// 4-arg route form:
server.route("POST", path, schema, handler)
```

- If the body is not valid JSON → **400** `{error: "validation failed", path: "", message: "body is not valid JSON"}` — handler not called.
- If the decoded value does not match the schema → **400** `{error: "validation failed", path, message}` — handler not called.
- On success → `req.body` is the **validated value** (type-coerced per the schema) and `req.rawBody` holds the **original JSON string**. The handler runs normally.

Requires the `data` Cargo feature (enabled by default). On a `--no-default-features` build, `schema` validation is silently skipped and `req.body` is the raw string as usual.

```ascript
import { create } from "std/http/server"
import * as schema from "std/schema"

let server = create()

const userSchema = schema.object({
  name: schema.string(),
  age: schema.number(),
})

server.post("/users", userSchema, req => {
  // req.body.name and req.body.age are type-checked values.
  // req.rawBody is the original JSON string.
  return { status: 201, body: "created " + req.body.name }
})
```

### The request object

Handlers and middleware receive a request object:

| field | type | meaning |
| --- | --- | --- |
| `method` | string | the HTTP method (uppercased). |
| `path` | string | the request path (without query). |
| `query` | object | parsed query string (`?a=1&b=2` → `{a:"1", b:"2"}`), percent-decoded. |
| `headers` | object | request headers, **keys lowercased**. |
| `params` | object | captured `:name` path params, percent-decoded. |
| `body` | string or validated value | the request body: a raw UTF-8-lossy string for plain routes; the schema-validated value for typed routes. |
| `rawBody` | string | **only on typed routes** — the original JSON string before schema validation. |

### Handler return conventions

A handler's return value is converted to a response:

- **a string** → `200`, `Content-Type: text/plain`.
- **an object** `{status?, headers?, body?}` → as specified (defaults: status `200`, empty body). `body` may be a string or bytes; a `text/plain` content-type is added if none was set and the body is non-empty.
- **a result pair** `[value, err]` → if `err` is non-nil, a `500` with the error message; otherwise the `value` is converted as above.

> [!NOTE] A handler or middleware **panic** (Tier-2) or a `?`-propagated error never kills the server — it is caught and converted to a `500` (the message is included for dev-friendliness), and the accept loop keeps serving. An **unmatched route** falls through to a `404` (middleware still runs first, so it can authenticate). Oversized headers → `431`; an oversized declared body → `413`; a read timeout → `408`.

### Example: middleware, params, and a JSON echo

```ascript
import { create } from "std/http/server"

let server = create()

// Auth-gate middleware: short-circuit with 401 unless a token is present.
server.use((req, next) => {
  if (req.headers.authorization == nil) {
    return ({ status: 401, body: "unauthorized" })
  }
  return next(req)
})

// A :id path param.
server.route("GET", "/users/:id", (req) => "user " + req.params.id)

// Echo the request body back as JSON.
server.route("POST", "/echo", (req) => ({
  status: 200,
  headers: { "content-type": "application/json" },
  body: req.body,
}))

// Bind an ephemeral port, then serve exactly 3 requests and stop.
let [port, berr] = await server.bind("127.0.0.1", 0)
print("listening on " + port)
await server.serve({ maxRequests: 3 })
```

## std/net/ws

WebSocket client and server handles, built on tokio-tungstenite. The server is **accept-based**, mirroring `std/net/tcp` and matching the single-threaded model — there is no `listen(host, port, handler)` callback form.

```ascript
import * as ws from "std/net/ws"
```

A connection is **message-oriented**: a string sends a Text frame and a Binary frame decodes to bytes; control frames (Ping/Pong) are handled transparently. A connection finalizes itself on a received Close frame or transport EOF, so use-after-close degrades gracefully (`recv` → `nil`; `send` → Tier-1 error).

### ws.connect

Opens a client WebSocket to a `ws://` or `wss://` URL. Async; returns `[conn, err]`.

- `url` (string) — `ws://…` or `wss://…` (TLS).
- `opts?` (object) — `opts.headers` (object of string→string) and `opts.auth` (`{bearer: tok}` or `{basic: [user, pass?]}`) are applied to the handshake request.
- Returns: `[conn, err]`.

```ascript
let [conn, err] = await ws.connect("wss://example.com/socket", {
  auth: { bearer: "my-token" },
  headers: { "x-client": "ascript" },
})
```

### ws.listen

Binds a TCP listener for accepting WebSocket connections. Async; returns `[listener, err]`.

- `host` (string), `port` (number, integer `0..=65535` — **port `0` is OS-assigned**, read `listener.port`).
- Returns: `[listener, err]`.

### Connection methods

- `await conn.send(data)` — a string sends a Text frame, bytes send a Binary frame. Returns `[nil, err]`; a send on a closed connection returns `[nil, err]`.
- `await conn.recv()` → `[message, err]` — a Text frame yields a string, a Binary frame yields bytes. A Close frame or transport EOF yields `[nil, nil]` (and finalizes the connection); Ping/Pong are handled transparently and skipped.
- `conn.close()` — sends a Close frame (best-effort) and finalizes the handle. Returns `[nil, err]`.

### Listener methods

- `listener.port` — the bound port (OS-assigned when you bound port `0`).
- `await listener.accept()` → `[conn, err]` — performs the TCP accept **and** the WebSocket handshake, returning the same kind of `conn` a client `connect` returns (so `send`/`recv`/`close` are identical on both ends).
- `listener.close()` — synchronous; stops accepting.

### Example: client

```ascript
import * as ws from "std/net/ws"

let [conn, err] = await ws.connect("ws://127.0.0.1:9001")
if (err != nil) { print(err.message) }

await conn.send("hello")
let [msg, rerr] = await conn.recv()
print(msg)

conn.close()
```

### Example: server accept-and-echo

> [!WARN] A full WebSocket round-trip needs the server's accept loop running while the client connects, so the server and client must run in **separate processes**. This snippet is the server side.

```ascript
import * as ws from "std/net/ws"

let [server, err] = await ws.listen("127.0.0.1", 0)
print("listening on " + server.port)

let [conn, aerr] = await server.accept()   // TCP accept + WS handshake
loop {
  let [msg, rerr] = await conn.recv()
  if (msg == nil) { break }                // peer closed
  await conn.send(msg)                      // echo it back
}

conn.close()
server.close()
```
