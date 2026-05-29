# AScript Milestone 14 — Async I/O Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §11.2 "Networking & servers" + the full §11.5 modern HTTP client: `std/net/tcp`, `std/net/http` (client — methods/headers/query/JSON/form/multipart/streaming bodies, redirects, decompression, timeouts, retries, cookies, TLS, proxy, HTTP/1.1+2(+3 feature-gated), streaming response bodies, and first-class Server-Sent Events `http.sse`), `std/http/server` (hyper), `std/net/ws` (WebSocket client + server). All async.

**Architecture — the key decision: NO new future/awaitable `Value` kind.** §7 + §11.5 show every async API awaited at its own call site (`await get(...)`, `await resp.json()`, `await resp.body.read()`, `await events.next()`); there is no "store a future, await later" or concurrent-spawn primitive. So the existing inline-async-dispatch model **already satisfies §7**: each async stdlib function / native-handle method `.await`s its work inside its own dispatch (like M13's `process.run`/`call_process_method`), and `ExprKind::Await(inner)` stays identity over the resolved value. **Network handles (TCP listener/stream, HTTP response, body reader, SSE stream, WS connection) are `Value::Native`** (the M13 mechanism) with async methods — add `NativeKind` + `ResourceState` variants + `call_<module>_method` handlers + cfg arms in `call_native_method`, exactly as sqlite/process did. The §11.4 reader idiom (`read(n?)`/`readLine()`/`readToEnd()`, finalize-on-EOF) IS the §11.5 streaming-body idiom — reuse it verbatim.

`std/http/server` is the architectural crux: hyper drives connections, but handlers are AScript functions needing `&mut Interp`, which is single-threaded (`Rc`/`RefCell`, current-thread tokio). Resolution: the server's `serve()` is an async interp method running an accept loop that handles requests **sequentially** through the single interp (`self.call_value(handler, [req], span).await` per request) — correct under the single-threaded model; true concurrent request handling is a documented v1 limitation (deferred).

**Tech Stack:** Rust 2021. New crates under a `net` feature (default-on): `reqwest` (rustls-tls, json, stream, multipart, cookies, gzip/brotli/deflate/zstd), `hyper` + `hyper-util` + `http-body-util` (server), `tokio-tungstenite` (rustls) (ws), `tokio` net/io features, `futures-util` (streams). HTTP/3 behind a nested `http3` feature (default-OFF, per §11.5 deferrals). Tests use **in-process fixtures** bound to `127.0.0.1:0` — NO external network.

**Starting state (end of M13, on `main`):** 374 tests default (245 `--no-default`), clippy clean. Resource-handle mechanism (`Value::Native`/`NativeMethod` + `resources` table + `call_native_method` cfg-dispatch) proven by sqlite/process; the §11.4 reader idiom + finalize-on-EOF are in place. `ExprKind::Await` is identity (interp.rs:708). Features: data/datetime/intl/sys/crypto/compress/sql.

**Conventions:** single-threaded `Rc`/`RefCell` (never Arc); `Control` Panic/Propagate; **Tier-1 for network failures** (connect/TLS/timeout/DNS = `err`; a non-2xx response is a NORMAL `resp` with `ok=false` unless `opts.errorOnStatus`); Tier-2 for arg-type misuse + use-after-close; cfg-gated registration; dual-config builds; `run`/`run_err` helpers; finalize stream resources on EOF (M13 fd-leak lesson).

## §11.5 surface (the contract — read §11.5 in full)

- **Entry points:** `get/post/put/patch/delete/head/options(url, opts?)` + `request(opts)` + `sse(url, opts?)`. All async → Tier-1 `[resp, err]` (sse → `[stream, err]`).
- **Request opts:** `method`, `url`/`query` (object→querystring), `headers` (object; `auth: {bearer}` / `{basic:[u,p]}` helpers), `body` (string · bytes · `{json:v}` · `{form:obj}` · `{multipart:[...]}` · `{stream:source}`), `timeout {connect,read,total}` ms, `redirect {follow,max}`|`"none"`, `retry {max,backoff,baseDelay,retryOn}`, `decompress` (default true), `tls {caBundle,clientCert,minVersion,sni,insecure}`, `cookies` (true|jar), `proxy` ("http://…"|"socks5://…"|"system"|"none"), `httpVersion` ("auto"|"1.1"|"2"|"3"), `stream` (true→streaming body), `bodyMode` ("string"|"bytes"), `errorOnStatus`, `cancel`.
- **Response:** `{status, ok(200-299), headers, version("1.1"|"2"|"3"), url(final), cookies}` + `await resp.text()/bytes()/json()` (buffered, each a Result) OR `resp.body` reader (streaming) + `resp.trailers` (best-effort).
- **SSE:** `sse(url,opts)→[stream,err]`; `await stream.next()→[{event,data,id,retry},err]` or nil at end; `stream.lastEventId`; auto-reconnect (default on, `reconnect:false` off; honors server `retry:` / `retryDefault` 3000ms; `maxReconnects`; sends `Last-Event-ID`); `stream.close()`. Parses `event:`/`data:`/`id:`/`retry:`, blank-line dispatch, multi-line `data:` joined with `\n`, `:`-comment lines ignored.
- **Deferrals (§11.5, carry forward):** HTTP/3 behind `http3` Cargo feature (default-off; `httpVersion:"3"` opt-in; clean error if feature off); response trailers best-effort; SOCKS proxy behind reqwest `socks` feature.

## Cargo features
```
net = ["dep:reqwest", "dep:hyper", "dep:hyper-util", "dep:http-body-util", "dep:tokio-tungstenite", "dep:futures-util", "dep:http", "tokio/net", "tokio/io-util"]
http3 = ["reqwest/http3"]   # default-OFF
```
`net` added to `default`. reqwest: `default-features=false, features=["rustls-tls","json","stream","multipart","cookies","gzip","brotli","deflate","zstd","socks"]` (socks on if it compiles cleanly cross-platform, per §11.5). Verify resolved versions; adapt APIs + report.

## Scope & Justified Deferrals
| Deferred | Why | Owner |
|---|---|---|
| HTTP/3 (QUIC) | reqwest `http3` is feature-gated + unstable-cfg; ships behind `http3` feature, default-off, `httpVersion:"3"` opt-in; auto/1.1/2 are the always-on baseline (§11.5) | M14 (feature-gated; promote when upstream stabilizes) |
| Response trailers | reqwest high-level API doesn't surface h2 trailers; `resp.trailers` best-effort (empty if unavailable) | M14 follow-up (hyper-level if first-class needed) |
| Concurrent request handling in `std/http/server` | single `&mut Interp`; sequential handling is correct under the single-threaded model | future (needs interior-mutability redesign or multi-interp) |
| `std/tui` | Terminal UI | M15 |
| LSP | Tooling | M16 |

---

## Task 1: `std/net/tcp` (foundational socket native handles)

**Files:** `Cargo.toml` (`net` feature + tokio net/io); modify `src/value.rs` (NativeKind variants), `src/interp.rs` (ResourceState + dispatch); create `src/stdlib/net_tcp.rs`; register cfg-gated.

API (§11.2 "listener + stream (connect, read, write)"): `connect(host, port)→[stream, err]` (async); `listen(host, port)→[listener, err]`; listener `accept()→[stream, err]` (async); stream `read(n?)/readLine()/readToEnd()`→chunk|nil (async, bytes by default OR string per a `mode`? — DECIDE: TCP is bytes-oriented; reads return bytes; provide `readLine` returning a string line for line-protocols, decode utf8 lossy), `write(data)`→[nil,err] (string/bytes), `close()`. listener `close()`.

- [ ] Add `NativeKind::{TcpListener, TcpStream}` (value.rs) + `ResourceState::{TcpListener(tokio::net::TcpListener), TcpStream(tokio::net::TcpStream + BufReader)}` (interp.rs, cfg `net`). Add the `#[cfg(feature="net")] TcpListener|TcpStream => self.call_tcp_method(...)` arm in `call_native_method`. (NativeKind variants always-compiled per M13 precedent; or cfg-gate — match the M13 approach: NativeKind always-present, ResourceState variants cfg-gated.)
- [ ] `src/stdlib/net_tcp.rs`: module fns `connect`/`listen`; `impl Interp { async fn call_tcp_method }` for stream read/write/close + listener accept/close. Reuse the M13 reader finalize-on-EOF + resource-table patterns. Register cfg-gated `net` in mod.rs.
- [ ] **Test fixture pattern (establish here for the whole milestone):** in tests, bind a tokio `TcpListener` to `127.0.0.1:0`, get the port, spawn a tokio task as the peer (echo server), then drive the AScript `connect`→write→read→close against it. e2e: AScript `listen` on `127.0.0.1:0` + `accept` in one task while a raw tokio client connects and sends — verify the AScript side reads it. Keep tests `#[tokio::test]`, localhost-only, deterministic (bound port via the listener's `local_addr()`).
- [ ] `cargo test` (default) + `--no-default-features` + clippy (both) + `build --no-default-features`. Commit `feat: std/net/tcp (listener + stream, async socket handles)`.

---

## Task 2: `std/net/http` client core + HTTP test fixture

**Files:** `Cargo.toml` (reqwest); modify value.rs (NativeKind::HttpResponse), interp.rs (ResourceState + dispatch); create `src/stdlib/net_http.rs`; register cfg-gated. Create a reusable in-process HTTP test server fixture (hyper or raw) in a test module.

Core surface: verbs `get/post/put/patch/delete/head/options(url, opts?)` + `request(opts)`. Build a `reqwest::Client` (per-call or cached). Map opts: `url`/`query` (object→`.query(&pairs)`), `headers` (object→header map; `auth:{bearer}`→Bearer, `{basic:[u,p]}`→basic), `body` string/bytes/`{json:v}` (serialize via the std/json converter or serde_json::Value from from_ascript)/`{form:obj}` (urlencoded)/`{multipart:[...]}` (form-data via `reqwest::multipart::Form`; each part `{name, value}` for a text field or `{name, filename?, data, contentType?}` for a file/bytes part — the `multipart` reqwest feature is enabled). Returns Tier-1 `[resp, err]`: a connect/TLS/DNS/timeout failure → `err`; otherwise a `Value::Native(kind=HttpResponse)` with `fields={status, ok, version, url}` + a headers object + cookies, and async methods `text()/bytes()/json()` (buffered → Tier-1 each). **Non-2xx is NOT an err** (resp with ok=false). 

- [ ] reqwest dep + `net` feature (the full feature list above). `connect`/request building. The response handle stores the `reqwest::Response` in `ResourceState::HttpResponse` (note: `Response::text()/bytes()/json()` consume the response — so `text/bytes/json` `take_resource` the response; calling a second body accessor → "body already consumed" Tier-2 panic or Tier-1 err — DECIDE: Tier-2 panic "response body already consumed"). `status`/`ok`/`version`/`headers`/`url` are read at response time into `fields`/a headers object (available without consuming).
- [ ] **HTTP test fixture:** a helper that starts an in-process server (use `hyper` + `hyper-util` since it's a dep for Task 6, OR a raw tokio responder) on `127.0.0.1:0` returning canned responses by path (e.g. `/json` → `{"x":1}`, `/echo` reflects method+headers+body, `/status/404` → 404, `/text` → "hello"). Tests drive the AScript client against `http://127.0.0.1:{port}/...`.
- [ ] Tests (via fixture): `get(url+"/text")` → resp.ok true, status 200, `await resp.text()`="hello"; `get(url+"/json")` → `await resp.json()` → object with x=1; `get(url+"/status/404")` → resp.ok false, status 404, err nil (NOT an err); `post(url+"/echo", {body:{json:{a:1}}})` → echo reflects the JSON body + content-type; `headers`/`auth` bearer sent + reflected; query params merged; a connect failure (port with nothing listening) → err non-nil; double body-consume → Tier-2 panic. interp e2e.
- [ ] `cargo test` + `--no-default-features` + clippy (both) + build. Commit `feat: std/net/http client core (verbs, headers, query, json/form body, response accessors)`.

---

## Task 3: HTTP client advanced options

**Files:** modify `src/stdlib/net_http.rs`.

Map the remaining §11.5 opts onto the reqwest client/request builder:
- [ ] `timeout {connect, read, total}` (ms); `redirect {follow, max}`|`"none"` (reqwest redirect policy); `retry {max, backoff:"exponential"|"constant", baseDelay, retryOn:[statuses]}` (hand-rolled retry loop around the request for connection errors + idempotent methods + retryOn statuses); `decompress` (default true — reqwest auto; set false to disable); `tls {caBundle, clientCert, minVersion, sni, insecure}` (rustls config — `insecure` disables verification, flagged); `cookies` (true→per-client jar; or a shared jar handle — a `Value::Native(kind=CookieJar)` OR just `true`); `proxy` ("http://…"|"socks5://…"|"system"|"none"); `httpVersion` ("auto"|"1.1"|"2"|"3" — "3" requires the `http3` feature else a clean Tier-1/Tier-2 error; pin via reqwest version opts; `resp.version` reports negotiated); `errorOnStatus` (non-2xx → err); `cancel` (a cancellation handle — `Value::Native(kind=CancelHandle)` whose `cancel()` aborts; OR map to a tokio CancellationToken stored in resources). 
- [ ] Tests via the fixture: timeout (fixture endpoint that delays → total-timeout err); redirect (fixture `/redirect`→`/text`, follow vs "none"); retry (fixture that fails N times then succeeds, OR returns a retryOn status then 200); errorOnStatus (404→err); auth basic; httpVersion "1.1"/"2" report via resp.version (h2 needs TLS — the fixture may be h1 only; test version reporting on what the fixture supports + that "3" without the feature errors cleanly); cookies round-trip (fixture sets a cookie, next request sends it). Mark genuinely-unsupportable-in-fixture cases (h2/proxy) as documented + minimally tested.
- [ ] Commit `feat: std/net/http advanced options (timeouts/redirects/retries/tls/cookies/proxy/httpVersion/cancel)`.

---

## Task 4: HTTP streaming bodies (response + request)

**Files:** modify value.rs (NativeKind::HttpBody), interp.rs (ResourceState), `src/stdlib/net_http.rs`.

- [ ] **Response streaming:** `opts.stream:true` → don't buffer; `resp.body` is a `Value::Native(kind=HttpBody)` reader following the EXACT §11.4 idiom: `await resp.body.read(n?)`→next chunk|nil at EOF, `readLine()`, `readToEnd()`; chunk type string|bytes per `opts.bodyMode` ("string" default | "bytes"). Backpressure-aware: each read awaits the next `reqwest::Response::chunk()` (pull-driven). Store the reqwest response's byte stream in `ResourceState::HttpBody` (a `futures_util::Stream` of `Bytes` + a leftover buffer for readLine/partial reads + bodyMode). Finalize on EOF (M13 lesson).
- [ ] **Request streaming:** `body:{stream:source}` where source is a `bytes` value, a std/process or fs reader (a `Value::Native` reader), or an async generator fn `() => [bytes, err]` (call repeatedly until nil). Send as a chunked/streamed request body via `reqwest::Body::wrap_stream`. (An AScript async-generator-fn source means calling a user fn repeatedly from within the request stream — feasible since we're on the single-threaded interp, but the reqwest Body stream is polled by reqwest's executor; bridging a user-fn-driven stream into reqwest may need an mpsc channel or pre-buffering. PRAGMATIC: support `bytes` and a reader-handle source first (read it fully or stream it); the async-generator-fn source MAY pre-buffer by calling the generator to exhaustion then sending — document if streamed-true-incremental from a user fn proves impractical on the single-threaded interp. Report the approach.)
- [ ] `call_http_body_method` for read/readLine/readToEnd. Tests via fixture: a large/chunked response streamed via read() yields chunks summing to the full body; readLine over a line-oriented response; bodyMode bytes; request streaming a bytes body the fixture echoes back. Commit `feat: std/net/http streaming response + request bodies`.

---

## Task 5: `http.sse` (first-class Server-Sent Events)

**Files:** modify value.rs (NativeKind::SseStream), interp.rs, `src/stdlib/net_http.rs`.

- [ ] `sse(url, opts?)→[stream, err]`: requests `Accept: text/event-stream`, consumes the response body as a stream. `Value::Native(kind=SseStream)` with `fields` and `lastEventId` (a readable field, updated as events arrive — store on the resource, expose via a method or refresh into fields). `await stream.next()→[{event(default "message"), data, id, retry}, err]` or nil when ended (and not reconnecting). Parse the SSE wire format: `event:`/`data:`/`id:`/`retry:` fields; dispatch a buffered event on each blank-line boundary; multi-line `data:` joined with `\n`; `:`-comment lines ignored. `stream.lastEventId`. **Auto-reconnect** (default on; `opts.reconnect:false` off): on disconnect wait server `retry:` (or `opts.retryDefault` default 3000ms), reconnect with `Last-Event-ID`, resume; `opts.maxReconnects` caps. `stream.close()` cancels + ends.
- [ ] `ResourceState::SseStream` holds the byte stream + a line/event parse buffer + lastEventId + reconnect config + the request template (for reconnect). `call_sse_method` for next/close.
- [ ] Tests via fixture: an SSE endpoint emitting a few `data:`/`event:`/`id:` frames + a multi-line data event + a comment line → `next()` returns the parsed events in order with correct event/data/id, then nil at stream end (reconnect off for the deterministic test); lastEventId tracks the last `id:`; a separate test for auto-reconnect (fixture drops then a second connection with `Last-Event-ID` resumes — or test reconnect-off ends cleanly). close() ends the stream. Commit `feat: http.sse first-class Server-Sent Events client`.

---

## Task 6: `std/http/server` (hyper)

**Files:** `Cargo.toml` (hyper/hyper-util/http-body-util); modify value.rs (NativeKind::HttpServer), interp.rs; create `src/stdlib/http_server.rs`; register cfg-gated `net`.

API (§11.2 "routes, handlers, middleware, params"): `create()`→a server-builder Native (or an object); `server.route(method, path, handler)` registers a handler (an AScript fn `(req) => resp`); `server.use(middleware)` registers middleware `(req, next) => resp`; path params (`/users/:id` → `req.params.id`); `server.listen(host, port)`→async, runs the accept loop (blocks). The handler receives a request object `{method, path, query, headers, params, body(string)}` and returns a response — an object `{status?, headers?, body?}` or a string (200 + body) or a Result.

**Architecture (the crux):** the server is created + routes/middleware registered (handlers stored as `Value`s in the server's resource state / fields). `listen()` is an async interp method: bind a `tokio::net::TcpListener`, accept connections in a loop, and for each request parse it (via hyper's HTTP/1 codec — `hyper::server::conn::http1` + `http_body_util`), match the route, build the request object, call `self.call_value(handler, [req], span).await` (running middleware chain → handler), convert the returned value to an HTTP response, write it back. **Handle requests SEQUENTIALLY** (await the handler fully before serving the next request/connection) — correct under the single `&mut Interp`. Document that concurrent request handling is a v1 limitation. To keep `listen()` interruptible/testable, accept a stop condition (e.g. a max-requests for tests, or a cancel handle).

- [ ] Implement with hyper's low-level `http1::Builder::serve_connection` driven by the interp's accept loop (NOT hyper's high-level `Server` which spawns tasks — we need single-threaded sequential control). For each connection, read the request, dispatch, respond. A `Value::Native(kind=HttpServer)` holds the routes/middleware (or store them in fields as arrays/objects of handlers).
- [ ] Tests: start an AScript server on `127.0.0.1:0` in a tokio task (or drive `listen` with a max-requests stop), then hit it with the std/net/http client (Task 2) — GET a route returns the handler's body; path params extracted; a POST with a body the handler echoes; middleware runs (e.g. adds a header / short-circuits); 404 for an unmatched route; a handler returning `{status:201, body:"created"}`. Use a request-count stop or a cancel so `listen` returns and the test completes. Commit `feat: std/http/server (hyper, routes/handlers/middleware/params, sequential)`.

---

## Task 7: `std/net/ws` (WebSocket client + server)

**Files:** `Cargo.toml` (tokio-tungstenite); modify value.rs (NativeKind::WsConnection), interp.rs; create `src/stdlib/net_ws.rs`; register cfg-gated `net`.

API (§11.2 "WebSocket client + server"): client `connect(url, opts?)→[conn, err]` (async, ws://|wss://); conn `send(data)→[nil,err]` (string→text frame, bytes→binary), `recv()→[message, err]` or nil on close (message = string for text, bytes for binary), `close()`. Server: `listen(host, port, handler)` where handler `(conn) => ...` is called per connection (sequential, like http server) OR an accept-based API `wsListen(...)→listener` + `accept()→[conn, err]`. DECIDE the server shape (accept-based is simpler + consistent with tcp); document.

- [ ] `NativeKind::WsConnection` (+ maybe `WsListener`); `ResourceState` holds the tokio-tungstenite `WebSocketStream`. `call_ws_method` for send/recv/close. Use `tokio_tungstenite::connect_async` (client) + `accept_async` over a tokio TcpStream (server).
- [ ] Tests: in-process — start a tungstenite echo server in a tokio task on `127.0.0.1:0`, AScript client `connect`→`send("hi")`→`recv()`="hi"→`close()`; binary frame round-trip; recv after close → nil; AScript ws server (accept) + a tungstenite client. Commit `feat: std/net/ws (WebSocket client + server)`.

---

## Task 8: Example + holistic + HTTP/3 feature gate + deferrals verification

**Files:** create `examples/net.as`; modify `tests/cli.rs`; `Cargo.toml` (`http3` feature, default-off).

- [ ] `examples/net.as`: a cohesive showcase using an in-... — actually examples run standalone without a fixture; an example that makes a real network call would be non-deterministic/offline-fragile. INSTEAD: make `examples/net.as` exercise the std/http/server + std/net/http client TOGETHER in one script (start a server on a port in the background via async, then the client hits it) — fully self-contained, no external network. OR keep the example to TCP loopback (listen + connect + echo in one script). Pick the self-contained loopback demo. RUN it; capture output; verify.
- [ ] `runs_net_example` in tests/cli.rs, gated `#[cfg(all(feature="net", unix))]` (or just `net`), asserting the loopback round-trip output.
- [ ] **`http3` feature (default-off):** add `http3 = ["reqwest/http3"]`; `httpVersion:"3"` returns a clean Tier-1/Tier-2 error when the feature is off ("HTTP/3 requires the 'http3' build feature"); document. Verify `cargo build` (default, no http3) compiles and the http3 path is feature-gated. (Do NOT enable http3 by default — it needs an unstable cfg.)
- [ ] **Deferrals doc:** ensure SOCKS proxy (if not cleanly compiling), response trailers (best-effort), and HTTP/3 are documented in code comments + the roadmap hand-off as §11.5-justified deferrals.
- [ ] Conformance: example parses under both parsers. FINAL: `cargo test` (default) + `cargo test --no-default-features` (net cfg's out) + `cargo clippy --all-targets` (both) + `cargo build --no-default-features` + `cargo build` (default). All green/clean/compile. Commit `test: net end-to-end example + http3 feature gate + deferrals doc`.

---

## Definition of Done

- `cargo test` (default) passes all suites (incl. the in-process network fixtures); `cargo clippy --all-targets` clean; `cargo test --no-default-features` passes + `cargo build --no-default-features` compiles (net cfg out).
- Implemented per §11.2 Networking + §11.5: `std/net/tcp`, `std/net/http` (full §11.5 client incl. streaming + SSE), `std/http/server`, `std/net/ws`.
- No new future kind (inline async dispatch); network handles are `Value::Native` with async methods reusing the M13 mechanism + reader idiom + finalize-on-EOF.
- Tier-1 for network failures (non-2xx = normal resp unless errorOnStatus); Tier-2 for arg misuse/use-after-close.
- §11.5 deferrals (HTTP/3 feature-gated default-off, trailers best-effort, SOCKS feature) documented with owners. Server's sequential-request-handling limitation documented.

## Hand-off to Milestone 15 ("Terminal UI") + M16 (LSP)

M15: `std/tui` (raw mode, alt screen, screen buffer, key/mouse events, basic widgets & drawing — `crossterm`/`ratatui`), under a `tui` feature. TUI is largely synchronous terminal I/O + an event loop (crossterm event read is blocking/pollable — may use a `tui`-gated reader); native handles for the terminal/screen. M16: `ascript lsp` (`tower-lsp`) over the shared front-end + the conformance-tested Tree-sitter grammar — reuses `SourceInfo`/ariadne diagnostics; the differential `frontend_conformance` guardrail + the grammar are the LSP's parsing backbone. After M14, the async/event-loop infrastructure is mature; M15/M16 are the last two milestones to "everything in the spec implemented."
