# AScript Container-Native Runtime + `std/docker` — Design (CNTR)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** CNTR (Deployment & reach track of the PERF campaign — see `goal-perf.md`)
- **Depends on:** FFI/caps (merged — the `required_cap` chokepoint + `CapSet` this spec extends),
  SRV (merged — the `accept_loop`/`Notify` stop machinery the graceful drain composes), workers
  (merged — the pool whose sizing goes cgroup-aware). **RT**
  (`2026-06-12-native-runtime-stubs-design.md` — specced, unshipped) is a dependency ONLY of the
  official-base-image deliverable (§9.1, tasks marked
  RT-DEPENDENT); **RESIL** (`2026-06-12-resilience-stdlib-design.md` — specced, unshipped) is a
  dependency ONLY of the template's
  circuit-breaker/rate-limit wiring (§9.3, marked RESIL-DEPENDENT). Everything else in this spec
  is executable today.
- **Depended on by:** the "AScript is a first-class container citizen" story: production servers
  that drain on SIGTERM, size themselves to their cgroup, supervise sibling containers, and ship
  as minimal images.
- **Engines:** both (tree-walker oracle == specialized VM == generic VM == `.aso`, byte-identical).
  The entire feature lives at the stdlib/`Interp`/CLI layer both engines share via `call_stdlib`;
  **no grammar change, no new `Value` variant, no opcode, no `ASO_FORMAT_VERSION` bump**
  (currently **27**, `src/vm/aso.rs:167` — pinned by the negative-space test).
- **Breaking:** none. Every existing program is byte-identical: capabilities stay
  default-all-granted (the dual-cap extension preserves the `all_granted()` short-circuit
  unchanged); `server.serve` without the new opts behaves exactly as today; the worker pool's
  default size only changes inside a cgroup-limited container (where today's `num_cpus` answer is
  *wrong* — see §8); `{socketPath}` is a new opt-in request option.

---

## 1. Summary & motivation

AScript ships multi-core HTTP servers (SRV), a real sandbox (FFI/caps), and single-binary
distribution (BIN/BNDL) — but it is a poor *container citizen*: a SIGTERM from the orchestrator
kills it mid-request (no inbound signal handling exists anywhere in the runtime — `std/process`
only *names* signals for children, `src/stdlib/process.rs:17`), the worker pool sizes itself by
`num_cpus::get()` (`src/worker/pool.rs:63`) which reads the **host's** core count inside a
cpu-quota'd cgroup (the classic container oversubscription bug), and it cannot talk to the
container substrate itself — no Unix-domain sockets in `std/net`, therefore no Docker Engine API,
therefore no supervisors, sidecars, CI tooling, or self-deploying programs written in AScript.

CNTR closes all of it in one coherent spec because the parts compose: **UDS** is the transport the
**Docker Engine API** rides; docker control is host-root-equivalent, which forces the **dual-cap
chokepoint extension**; a container-native server needs **inbound signals** wired to **graceful
drain**; and a container-native *runtime* needs **cgroup-aware sizing** plus the distribution half
(base images, a Deploying chapter, `ascript init --template server`).

### Design tenets locked up front

1. **The docker socket is root.** Anyone who can POST to `/containers/create` can bind-mount `/`
   and spawn arbitrary host processes. So `docker.*` requires **BOTH `net` AND `process`** — either
   `--deny net` or `--deny process` (or `--sandbox`, or a reduced-`CapSet` isolate) blocks every
   docker call. This is the spec's security core (§5) and the reason the one-cap
   `required_cap(module, func) -> Option<Cap>` chokepoint grows a minimal multi-cap return type —
   while preserving the Gate-12 `all_granted()` zero-cost short-circuit byte-for-byte.
2. **One HTTP machinery seam, not two response surfaces.** `{socketPath}` requests return the SAME
   `[resp, err]` / `resp.text()` / `resp.json(Class)` / streaming `resp.body` surface as TCP
   requests. Internally they do NOT go through reqwest (§3.2 explains why reqwest is the wrong
   tool here); they go through a minimal, hardened HTTP/1.1 client whose body is surfaced as the
   exact `ByteStream` type `net_http`'s shipped `StreamingBody` machinery already consumes — so
   the streaming/decode/backpressure code is reused verbatim, not reimplemented.
3. **Unix-only, loudly.** UDS and `std/docker` are Unix-only in v1. On Windows every entry point
   raises a clear Tier-2 error (`Unix-domain sockets are not supported on this platform`); Windows
   named pipes (`npipe://./pipe/docker_engine`) are recorded as future work (§12), never silently
   half-supported.

## 2. The six parts at a glance

| Part | What it adds | Where it lives |
|---|---|---|
| **UDS foundation** (§3) | `std/net/unix` connect/listen handles; `std/http` `{socketPath}` over a minimal hardened HTTP/1.1 client (chunked, streaming, Upgrade) | new `src/stdlib/net_unix.rs`, new `src/stdlib/http1.rs`; `net_http.rs` routing |
| **`std/docker`** (§4) | typed Engine-API wrapper: containers/images/exec; `logs`/`events`/`pull` as `for await` streams; the 8-byte attach demux | new `src/stdlib/docker.rs` (`docker` feature, default-on); 2 new `NativeKind`s |
| **Dual-cap gate** (§5) | `CapReq` bitset return for `required_cap`; `governing_caps` per-handle re-check; cap-audit rows | `src/stdlib/{caps,mod}.rs`, `src/value.rs`, `src/interp.rs`, `tests/cap_audit.rs` |
| **Inbound signals** (§6) | `process.on("SIGTERM", h)` / `process.off`; tokio::signal; main-isolate-only | `src/stdlib/process.rs`; `tokio/signal` feature |
| **Graceful drain** (§7) | `server.serve({onShutdown, drainTimeout})` + `srv.shutdown()`; composes with multi-isolate + signals | `src/stdlib/http_server.rs` |
| **Container-native runtime + distribution** (§8–§9) | cgroup-aware pool sizing (v1+v2), `os.inContainer()`, base images (RT-dep), Deploying docs, `ascript init --template server` | `src/worker/pool.rs`, `src/stdlib/os.rs`, `src/main.rs`, `docs/`, `templates/` |

## 3. The UDS foundation

### 3.1 `std/net/unix` — connect + listen handles

A new module `std/net/unix` (registered in `STD_MODULES`, routed in both `call`/`exports` arms of
`src/stdlib/mod.rs`, module key `net_unix`), deliberately a **structural mirror of
`src/stdlib/net_tcp.rs`** — same Tier-1 `[handle, err]` entry points, same buffered-stream method
set, same EOF-finalize/take-out-across-await discipline:

```
import * as unix from "std/net/unix"

let [stream, err] = await unix.connect("/var/run/docker.sock")   // [UnixStream, err]
await stream.write("GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n")
let line = await stream.readLine()

let [srv, lerr] = unix.listen("/tmp/app.sock")                   // [UnixListener, err]
let [conn, aerr] = await srv.accept()
```

- `connect(path) -> [stream, err]` — `tokio::net::UnixStream::connect`. The stream handle exposes
  exactly the TCP stream's methods: `read(n?)`, `readLine()`, `readToEnd()`, `write(data)`,
  `close()` — implemented over the same `BufReader` shape as `TcpStreamState`
  (`src/stdlib/net_tcp.rs:31`), including the `read(0)`-no-finalize and EOF-drops-fd rules and
  their tests, mirrored.
- `listen(path) -> [listener, err]` — `tokio::net::UnixListener::bind`. The handle's `fields`
  carry `path`. **Stale-socket rule:** bind to an existing path fails with `EADDRINUSE`; that is
  surfaced as the Tier-1 err (no silent unlink — deleting a file the program may not own is a
  policy decision the caller makes with `fs.remove`). On `close()` / handle drop the listener fd
  closes; the socket *file* is unlinked by the listener's `Drop` (tokio does not unlink; we wrap
  the listener in a small struct whose `Drop` best-effort `unlink`s the path it created —
  deterministic cleanup per the native-resource rule).
- **`NativeKind::UnixStream` / `NativeKind::UnixListener`** — new variants: GC-untraced (no-op
  `Trace`, the native-resource rule), non-sendable (the worker airlock's existing field-path
  panic), `governing_caps` = `net` (§5.3), registered in every exhaustive `NativeKind` match
  (`type_name`, `governing_cap(s)`, the serializer's non-sendable arm).
- **Capability:** `net_unix` maps to `Cap::Net` in `required_cap` — gated **by construction** at
  the `call_stdlib` chokepoint (`src/stdlib/mod.rs:325`), exactly like `net_tcp`. The `net`
  *carve-out* (host allow-lists, `check_net_host`) does NOT apply — a UDS path is not a host;
  under a configured net carve-out (`net_scope: Some`), the dispatch decision is `Defer` and the
  UDS stage-2 check **denies** (`capability 'net' denied for unix socket '<path>'`) unless the
  literal path is on the allow list (entries like `unix:/var/run/docker.sock`). Rationale: a
  carve-out is "deny net, allow back named endpoints"; a UDS connect is a net endpoint and must
  not slip through a host-keyed allow list as an unchecked default-allow.
- **Windows:** the module routes on all platforms; on non-Unix every fn raises Tier-2
  `Unix-domain sockets are not supported on this platform` (documented; the internals are
  `#[cfg(unix)]`). This matches the SRV `reuseport_available` precedent of honest platform gating.

### 3.2 `std/http` `{socketPath}` — and why NOT reqwest

`http.request({ socketPath, path, method, headers, body, stream })` and the verb helpers
(`http.get(url, { socketPath })` — `url`'s host is ignored-but-must-parse; the canonical form is
`request` with `path`) route the request over a Unix socket.

**Decision (made after reading `net_http.rs` end to end): `{socketPath}` requests do NOT use
reqwest.** Three reasons, each individually sufficient:

1. reqwest's stable public API has **no Unix-socket connector seam** — `Client` resolves and
   connects TCP. (hyper can do UDS with a custom connector / `hyper::client::conn::http1`
   handshake over any `AsyncRead+AsyncWrite`, but reqwest does not expose that path.)
2. The Docker exec/attach protocol requires a **connection Upgrade hijack** (`Connection:
   Upgrade` / `Upgrade: tcp`, then raw bidirectional frames on the same socket). reqwest's
   response API cannot hand back the underlying connection. We need the raw stream + leftover
   buffered bytes after the response head — only our own client can give us that.
3. The Engine API is plain HTTP/1.1 with chunked transfer — a deliberately tiny, fully
   specifiable surface. A minimal client is *less* total risk than threading an unstable
   connector through reqwest.

So: a new **`src/stdlib/http1.rs`** — a minimal, hardened HTTP/1.1 **client codec** used ONLY for
`socketPath` requests (TCP requests keep reqwest, byte-identical):

- **Request writer:** request line + headers (always sends `Host: localhost` — the Engine API
  requires *a* Host header, value irrelevant over UDS; `Connection: close` by default, v1 has no
  keep-alive pooling — recorded future perf work §12), body as `Content-Length` (buffered) —
  request streaming over UDS is out of v1 scope (Tier-2 clear error if `body.stream` is combined
  with `socketPath`; documented).
- **Response parser** (the trust boundary — Gate-14 hardening applies in full): status line
  (HTTP-version validated, status 100–599), header block with caps (max 256 headers, 64 KiB
  header-block total, malformed → Tier-1 err, never a panic/overflow), then the body framed by
  `Content-Length` (length-checked, capped against the same `MAX_ALLOC_COUNT` discipline the
  stdlib uses) or **`Transfer-Encoding: chunked`** (hex size line with cap, CRLF discipline
  enforced, 0-chunk + trailer skip), or read-to-EOF (`Connection: close` responses).
- **The body is surfaced as `net_http`'s own `ByteStream`** (`Pin<Box<dyn Stream<Item =
  io::Result<Bytes>>>>`, `src/stdlib/net_http.rs:145–170`): the chunked/CL decoder is written as
  a stream adapter over the `UnixStream`, so `StreamingBody`, `BodyMode`, the `HttpBody` native
  kind, `read/readLine/readToEnd`, and the backpressure model are **reused verbatim** —
  `opts.stream:true` over UDS returns the same `resp.body` handle the reqwest path returns.
- **Buffered path:** without `stream:true`, the body is drained and the response surfaces as the
  same response shape the reqwest path produces: `resp.status` (int), `resp.headers` (object),
  `resp.text()`, `resp.json(Class?)` (the shipped fused typed-decode). Implementation detail: the
  UDS response registers the same `NativeKind::HttpResponse` resource with a parallel
  `ResourceState` arm (the state enum is internal; the script-visible surface is identical and
  asserted identical by tests).
- **Upgrade takeover:** `http1::send` returns, on a `101`/upgrade response, the raw
  `UnixStream` + any bytes already buffered past the response head — the handoff `std/docker`'s
  attach/exec demux consumes (§4.4). Not script-visible in v1 (no `http`-level upgrade API).
- **Capability:** the `socketPath` branch lives inside `call_http` → already `Net`-gated at the
  chokepoint by construction; the UDS stage-2 carve-out rule of §3.1 applies (the same
  `unix:<path>` allow-list check, shared helper).

## 4. `std/docker` — the typed Engine API wrapper

New module + Cargo feature **`docker = ["net"]`**, in `default` (batteries-included; the cap gate
— not feature subtraction — is the safety story, the FFI §3.6 argument verbatim). Unix-only at
runtime (§3.1 rule). `import * as docker from "std/docker"`.

### 4.1 Connection + version negotiation

```
let [d, err] = await docker.connect()                 // default socket
let [d2, e2] = await docker.connect({ socketPath: "/run/user/1000/docker.sock" })
```

- Socket resolution order: `opts.socketPath` → `$DOCKER_HOST` **iff** it is a `unix://` URL
  (a `tcp://` DOCKER_HOST is a Tier-1 err naming the limitation — never a silent fallback) →
  `/var/run/docker.sock`. Rootless docker therefore *works* via explicit `socketPath`/
  `DOCKER_HOST`; its nuances (different default path per uid) are documented, not special-cased
  (§12).
- `connect` performs **version negotiation**: `GET /v{FLOOR}/version` (FLOOR = `1.24`, the oldest
  API every supported daemon answers), reads `ApiVersion`, and pins the client to
  `min(daemon ApiVersion, CEILING)` with CEILING = `1.43` (the newest API this wrapper is written
  against). Every subsequent request uses the negotiated `/v{n}/` base path; the negotiated
  version is readable as `d.apiVersion` (a handle field). A daemon older than FLOOR is a Tier-1
  err. A failed/unreachable socket is a Tier-1 err (probing for docker is legitimate).
- The client handle is **`NativeKind::DockerClient`** backed by `ResourceState::DockerClient
  { socket_path, api_version }` — v1 opens **one fresh `UnixStream` per request**
  (`Connection: close`); correctness-first, keep-alive recorded as future work (§12). `d.close()`
  drops the handle state (idempotent); streams opened from it hold their own connections and
  outlive it independently (each stream owns its fd — deterministic per-stream cleanup).

### 4.2 Containers & images (unary calls — all Tier-1 `[value, err]`)

House convention: **I/O and API errors are Tier-1 pairs** (a 404 "no such container" is data you
handle: `err.message` + `err.statusCode`); **argument misuse is Tier-2** (non-string id, bad opts
type — the stdlib-wide rule). JSON responses decode with the shipped `json` machinery; key names
are passed through as the Engine API returns them (`Id`, `Names`, `State`, …) — no renaming layer
to drift.

| Call | Engine API | Returns |
|---|---|---|
| `d.ping()` | `GET /_ping` | `[true, err]` |
| `d.version()` / `d.info()` | `GET /version` / `GET /info` | `[object, err]` |
| `d.containers(opts?)` | `GET /containers/json` (`all`, `filters` → JSON-encoded query) | `[array<object>, err]` |
| `d.inspect(id)` | `GET /containers/{id}/json` | `[object, err]` |
| `d.create(config, name?)` | `POST /containers/create` (config passed through as the API's JSON body) | `[id, err]` |
| `d.start(id)` / `d.restart(id, opts?)` | `POST .../start` / `.../restart` (`t` timeout) | `[nil, err]` |
| `d.stop(id, opts?)` | `POST .../stop` (`t`, `signal`) | `[nil, err]` |
| `d.remove(id, opts?)` | `DELETE /containers/{id}` (`force`, `v` volumes) | `[nil, err]` |
| `d.wait(id)` | `POST .../wait` (blocks until exit) | `[statusCode int, err]` |
| `d.images(opts?)` | `GET /images/json` | `[array<object>, err]` |
| `d.removeImage(ref, opts?)` | `DELETE /images/{ref}` (`force`) | `[array<object>, err]` |

Empty-body 204s resolve `[nil, nil]`. Non-2xx responses parse the daemon's `{"message": …}` JSON
into the err object (`{message, statusCode}`); an unparseable error body degrades to the raw text.

### 4.3 Streams — `logs`, `events`, `pull` (the `for await` surface)

```
let [logs, err] = await d.logs(id, { follow: true, stdout: true, stderr: true, tail: 100 })
for await (entry in logs) {
    print("[${entry.stream}] ${entry.text}")
}
logs.close()
```

All three return **`NativeKind::DockerStream`** — the SSE pattern mirrored exactly
(`SseStream`, `src/stdlib/net_http.rs` / `src/interp.rs:7713`): a native handle whose
`await stream.next() -> [item, err]` contract ends with a `nil` item, registered in
`native_stream_method` (→ `"next"`), which makes it `for await`-iterable **on both engines for
free** (the tree-walker's `exec_for_await` `Value::Native` arm at `src/interp.rs:5090` and the
VM's shared path both consult `native_stream_method`). `stream.close()` drops the dedicated
connection (and the in-flight HTTP read with it — deterministic fd reclaim). The stream holds its
own `UnixStream`; reads are pull-driven (backpressure to the daemon, the `StreamingBody` model).

- **`d.logs(id, opts?)`** — `GET /containers/{id}/logs` (`follow`, `stdout`, `stderr`, `tail`,
  `since`, `until`, `timestamps`). Items are objects `{stream: "stdout"|"stderr", text: string}`
  (UTF-8-lossy; `opts.bytes: true` yields `data: bytes` instead — the `BodyMode` precedent).
  Framing is the **attach multiplex demux** (§4.4) when the container has no TTY; for a TTY
  container the daemon sends a raw byte stream — the demux **auto-detects** per the protocol
  (§4.4) so callers never pass a tty flag.
- **`d.events(opts?)`** — `GET /events` (`since`, `until`, `filters` JSON-encoded). The body is a
  chunked stream of newline-delimited JSON objects; each item is the decoded object. Never ends
  unless `until` is set or the stream is closed — `for await` + `break`/`close()` is the
  consumption model.
- **`d.pull(ref)`** — `POST /images/create?fromImage=…&tag=…`. The body is JSON-lines progress
  (`{status, progressDetail, id}`); each decoded object is an item; stream end = pull complete.
  A registry error arrives as an in-stream `{"error": …}` line → surfaced as the `[nil, err]`
  terminal item (then end), matching the daemon's protocol.

### 4.4 The 8-byte multiplex demux (logs / exec / attach)

When the target has no TTY, the daemon multiplexes stdout/stderr onto one connection with frames:

```
[ STREAM_TYPE: u8 | 0 | 0 | 0 | SIZE: u32 big-endian ]  then SIZE payload bytes
   0=stdin 1=stdout 2=stderr
```

The demux is one well-tested function in `docker.rs`:

- Reads exactly 8 header bytes (EOF mid-header after ≥1 byte = Tier-1 truncated-stream err; EOF
  at a frame boundary = clean end), validates `STREAM_TYPE ∈ {0,1,2}` and the three zero bytes.
- **TTY auto-detection:** if the first 8 bytes do not validate as a frame header (byte 0 ∉
  {0,1,2} or bytes 1–3 nonzero — true for any text payload), the whole stream is treated as a raw
  TTY stream: items are `{stream: "stdout", text: chunk}`. This is the documented best-effort
  heuristic the docker CLI itself relies on structurally (a TTY stream cannot collide with a
  valid header in practice; the alternative — an inspect round-trip per logs call — is recorded
  as the rejected design).
- `SIZE` capped (16 MiB/frame) against a hostile/corrupt daemon — over-cap is a Tier-1 err,
  never an allocation bomb (the `.aso`-reader-clamp lesson applied to a new untrusted-bytes
  boundary).

### 4.5 Exec

```
let [res, err] = await d.exec(id, { cmd: ["ls", "-l", "/"], workdir: "/", env: ["K=V"] })
// res = { exitCode: int, stdout: string, stderr: string }

let [execId, e1] = await d.execCreate(id, { cmd: [...], attachStdout: true, attachStderr: true })
let [stream, e2] = await d.execStart(execId)        // DockerStream of demuxed frames
let [info,  e3] = await d.execInspect(execId)       // { ExitCode, Running, ... }
```

- `execCreate` → `POST /containers/{id}/exec`; `execStart` → `POST /exec/{id}/start` with the
  **Upgrade hijack** (`Connection: Upgrade`, `Upgrade: tcp`): the §3.2 http1 takeover hands the
  raw `UnixStream` + leftover bytes to the §4.4 demux → a `DockerStream`. `execInspect` →
  `GET /exec/{id}/json`.
- `d.exec(...)` is the convenience composition: create → start → drain the demuxed stream into
  `stdout`/`stderr` accumulators → inspect for `exitCode`. The shape mirrors
  `process.run`'s result object deliberately.
- **v1 scope:** no interactive stdin attach (`attachStdin` is rejected with a clear Tier-2 error
  naming the deferral); recorded in §12.

### 4.6 Workflow determinism

`docker.*` inside a `workflow` body is nondeterministic I/O like any network call: the existing
`workflow-determinism` lint must flag it. Task-level requirement: verify the rule's
nondeterministic-call classification catches `docker.*` member calls on the imported module (it
classifies by stdlib module list — add `docker` to that list if list-based, with a rule test).
Record/Replay (SP9) does NOT get a docker seam in v1 — docker calls in deterministic mode are the
same documented exclusion class as general net I/O (the det context only seams RNG/clock/FFI).
Stale-proofing: once REPLAY (`2026-06-12-record-replay-design.md`) lands, the det seams extend to
fs/env/process/http and REPLAY's completeness enumeration will classify `docker`/`net_unix`
(expected: Refused) — coordinate then.

## 5. Dual-cap gating — the chokepoint extension (the security core)

### 5.1 Why both caps

The docker socket is **host-root-equivalent**: `/containers/create` with a
`Binds: ["/:/host"]` + `/exec` is arbitrary host file access AND arbitrary host process
execution. Gating it under `net` alone would mean `--deny process` (the "no subprocesses" stance)
still permits the strictly-stronger ability to run processes *via the daemon*. So `docker.*`
requires **`net` ∧ `process`** — the conjunction of the two authorities it actually conveys.
`std/net/unix` itself stays single-cap `net` (a UDS byte pipe conveys no process authority).

### 5.2 The minimal mechanism: `CapReq`

Today `required_cap(module, func) -> Option<caps::Cap>` (`src/stdlib/mod.rs:325`) returns ONE
cap and the gate is:

```rust
let cap_bits = self.caps_bits();              // Copy snapshot
if !cap_bits.all_granted() {                  // Gate-12: single flag, hot path untouched
    if let Some(cap) = required_cap(module, func) {
        self.require_cap(cap, module, func, args, span)?;
    }
    ...
}
```

**Decision: change the return type to a tiny `Copy` bitset, not a second lookup.** A parallel
`required_cap_extra` table would be a drift hazard (two enumerations to keep complete — the exact
failure mode the FFI completeness sweep exists to prevent); the bitset keeps ONE total
enumeration and reuses the existing `Cap::bit()` machinery:

```rust
// src/stdlib/caps.rs — beside CapBits.
/// A REQUIREMENT set: which capabilities a stdlib (module, func) needs — ALL of
/// them (conjunction). `Copy` u8 over the same bit layout as CapBits. Almost every
/// entry is a single cap; `docker` is the first conjunction (CNTR §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapReq(u8);

impl CapReq {
    pub const NONE: CapReq = CapReq(0);
    pub const fn one(cap: Cap) -> CapReq { CapReq(cap.bit()) }
    pub const fn and(self, cap: Cap) -> CapReq { CapReq(self.0 | cap.bit()) }
    pub const fn is_empty(self) -> bool { self.0 == 0 }
    /// Iterate required caps in the stable `Cap::ALL` order (fs, net, process, ffi,
    /// env) — the order fixes WHICH denial fires when several are denied.
    pub fn iter(self) -> impl Iterator<Item = Cap> {
        Cap::ALL.into_iter().filter(move |c| self.0 & c.bit() != 0)
    }
}
```

- `required_cap(module, func) -> CapReq` (same name, same single match — every existing arm
  becomes `CapReq::one(...)`, the fallthrough becomes `CapReq::NONE`, and the new arm is
  `"docker" => CapReq::one(Cap::Net).and(Cap::Process)`).
- The gate body becomes a loop — **only reachable when something was already dropped**, so the
  Gate-12 `all_granted()` short-circuit is preserved *unchanged at the instruction level* on the
  default path (the same single `Copy`-bitset flag test; the discipline DBG/FFI established):

```rust
if !cap_bits.all_granted() {
    for cap in required_cap(module, func).iter() {
        self.require_cap(cap, module, func, args, span)?;   // first denied wins
    }
    ...
}
```

- **Denial determinism:** caps are checked in `Cap::ALL` order, so under `--deny net` docker
  fails `capability 'net' denied`, under `--deny process` it fails `capability 'process' denied`,
  and under both, `net` (earlier in the order) fires — pinned by tests. The denial message format
  is the shipped one, unmodified (cap_audit asserts exact strings).
- **Carve-out interplay:** `require_cap`'s `Defer` (a configured `fs`/`net` carve-out) continues
  to pass the dispatch gate per-cap; for docker a `net` carve-out defers to the §3.1 UDS stage-2
  path check while the `process` bit is still required outright. (A carve-out never weakens the
  conjunction — each cap is evaluated independently.)
- **Completeness tests:** the existing `required_cap_complete_enumeration` /
  every-module-classified sweep (`src/stdlib/mod.rs:991+`) update mechanically
  (`Some(Cap::Fs)` → `CapReq::one(Cap::Fs)`); `docker` gets an explicit
  both-bits assertion; the `__probe__` classification test treats `is_empty()` as ungated.

### 5.3 Per-handle re-check: `governing_caps`

The FFI BLOCKER-3 rule — a handle opened pre-drop is denied post-drop — must hold for the new
handles. `NativeKind::governing_cap() -> Option<Cap>` (`src/value.rs`) becomes
**`governing_caps() -> CapReq`** (one method, same total enumeration, same mechanical
`CapReq::one` migration); `call_native_method`'s re-check loops the same way the dispatch gate
does, behind the same `all_granted()` short-circuit. New rows: `UnixStream`/`UnixListener` →
`net`; `DockerClient`/`DockerStream` → `net ∧ process`. So `caps.drop("process")` kills a live
docker client *and* its open log streams on their next method call — audited (§10.2).

### 5.4 Workers

Nothing new is needed: a reduced-`CapSet` **dedicated isolate** (`run_in_worker(f, x,
{caps:{deny:["process"]}})`) already carries its `CapSet` across the spawn; the docker conjunction
is evaluated against the isolate's own set. Docker handles are non-sendable natives → crossing
the airlock is the existing field-path panic. Pooled `worker fn`s inherit full caps as today.

## 6. Inbound signals — `process.on` / `process.off`

### 6.1 Surface & semantics

```
import * as process from "std/process"

process.on("SIGTERM", () => {
    log.info("draining")
    srv.shutdown()
})
process.on("SIGINT", handleInt)     // replaces a previous SIGINT handler (last-wins)
process.off("SIGTERM")              // remove the handler; default termination restored (§6.3)
```

- **Signals accepted (Unix):** `SIGTERM`, `SIGINT`, `SIGHUP`, `SIGQUIT`, `SIGUSR1`, `SIGUSR2` —
  the `tokio::signal::unix::SignalKind` set a server legitimately handles. `SIGKILL`/`SIGSTOP`
  are uncatchable → Tier-2 error naming that fact. Unknown name → Tier-2 (the `caps.drop`
  unknown-name precedent). On **Windows**, `"SIGINT"` maps to `tokio::signal::ctrl_c`; every
  other name is a clear Tier-2 `signal 'X' is not supported on this platform`.
- **One handler per signal, last-wins** (`process.on` twice replaces). The handler is any
  callable; it is invoked with one arg — the signal name string.
- **Registration replaces default termination for that signal** (precise statement): before the
  first `process.on("SIGTERM", …)`, SIGTERM kills the process (OS default). After it, SIGTERM
  invokes the handler and the process **does not terminate** unless the handler makes it (e.g.
  resolves the program / calls `srv.shutdown()` so `serve` returns / `process.exit`). Repeated
  signals re-invoke the handler (tokio's stream semantics; coalescing of bursts is the documented
  OS behavior).
- **Handler execution:** an eager-scheduled ordinary task — registration spawns ONE listener task
  per signal (`spawn_local`, the owned-`Rc<Interp>`-via-`self.rc()` pattern `task.spawn` uses,
  `src/stdlib/task_mod.rs`) looping on `signal.recv()`; each receipt calls the handler via
  `call_value` exactly like a `task.spawn` callback — same engine-shared machinery, so both
  engines are byte-identical by construction. A handler panic is reported like a panicking
  spawned task (loud, does not kill the listener loop). The listener task holds only a `Weak`
  interp (the `self.rc()` discipline) and never blocks program exit: when the main program
  resolves, the runtime tears down as today (the listener is a LocalSet task that dies with it) —
  signal handlers do NOT keep the program alive; a server keeps itself alive by `await serve(…)`.
- **Cargo:** add `signal` to the tokio feature list (`Cargo.toml:32` — the current features are
  `rt, rt-multi-thread, macros, time, sync`; `tokio::signal` on the `current_thread` runtime
  needs the signal driver, enabled by the `#[tokio::main]` default `enable_all`). `process.on`
  ships under the existing `sys` feature with the rest of `std/process`.

### 6.2 Main-isolate only

Signals are **process-global**; N isolates each installing handlers would be N racing handlers
for one delivery. Rule: `process.on`/`off` in a **worker isolate** (pooled or dedicated) raises
Tier-2 `process.on is only available on the main isolate (signals are process-global)` — the
`caps.drop`-in-pooled-worker refusal pattern (`guard_drop_allowed`, `src/stdlib/caps.rs:888`)
reused with the same isolate-classification plumbing. Documented in `workers.md`.

### 6.3 `process.off` — the honest restore

tokio (and the underlying signal machinery) **cannot restore the OS default disposition** once a
handler is installed for the process lifetime. We do not pretend otherwise: `process.off(sig)`
removes the script handler, and the still-installed listener loop reverts to **emulated default
termination** — on the next delivery of that signal it exits with the conventional code
`128 + signo` (130 for SIGINT, 143 for SIGTERM) after flushing output, which is observably the
default behavior for the termination signals we accept. This emulation is stated in the docs
verbatim (the one observable difference — exit raced against in-flight output — is documented).

### 6.4 Determinism — documented exclusion

Signal arrival is wall-clock-external nondeterminism. **Decision: OUT of SP9 Record/Replay
scope** — recording a `DetEvent::Signal` would imply replay can re-deliver it at the same
*logical* point, which the het-async scheduler cannot guarantee (the M17 "deterministic task
scheduling" architectural non-goal); a half-faithful replay is worse than a loud exclusion. So:
(a) `det.rs` doc-comment + the workflow docs name the exclusion; (b) the `workflow-determinism`
lint flags `process.on` inside a workflow body (same mechanism that flags other
nondeterministic calls — rule test included; 0 FP on `examples/**`).

## 7. Graceful drain — `server.serve({onShutdown, drainTimeout})` + `srv.shutdown()`

### 7.1 Surface

```
let srv = server.create()
srv.get("/healthz", (req) => ({ status: 200, body: "ok" }))
process.on("SIGTERM", () => srv.shutdown())
let [_, err] = await srv.serve({ port: 8080, workers: 0,
                                 onShutdown: () => log.info("drain started"),
                                 drainTimeout: 8000 })
// serve resolves after: accept stopped → onShutdown ran → in-flight drained (≤ 8s)
```

- **`srv.shutdown()`** — a new server-handle method (sync, instant): fires the serve loop's stop
  `Notify`. Idempotent. Called before `serve` starts → the next `serve` returns immediately after
  running `onShutdown` (a pre-armed flag on the server resource; deterministic, documented).
- **`onShutdown`** — optional callable, invoked exactly ONCE on the serving isolate's main side
  when shutdown begins (after accepting stops, before the drain wait). An error/panic in it is
  reported and does not abort the drain.
- **`drainTimeout`** (ms) — optional. Absent = wait for ALL in-flight requests (no timeout).
  Present = after stopping accepts, wait up to `drainTimeout` for in-flight handlers to finish;
  any still running are then **aborted** (their tasks cancelled → connections close; the client
  sees a reset — stated plainly in docs). `serve` then resolves `[nil, nil]` (a drain timeout is
  an operational policy outcome, not an error; the abort count is `warn`-logged).

### 7.2 Mechanics (composing the SRV machinery, not duplicating it)

`accept_loop` (`src/stdlib/http_server.rs:1276`) already has everything but the trigger:

- **The stop `Notify` becomes always-armed.** Today the notify-select runs only when `bounded`
  (`maxRequests` set); the unbounded path awaits `accept()` directly "byte-identical to the old
  loop". Generalize the select guard from `bounded` to `bounded || stoppable` where `stoppable`
  is true whenever a server handle exposing `shutdown()` exists — i.e. always for `serve`. This
  is **behaviorally identical** when `shutdown()` is never called (the `Notify` never fires; the
  biased select takes the accept arm), and the SRV lost-wakeup discipline (register-enable-then-
  recheck, `http_server.rs:1311–1332`) is reused verbatim with a second wake condition
  (`shutdown_flag` checked beside the budget). The four-mode differential + the existing server
  tests are the proof it stays identical.
- **In-flight tracking with bounded memory.** Today `inflight: Vec<JoinHandle>` is retained only
  when bounded (else handles are dropped to avoid unbounded accumulation). New rule: handles are
  retained **always**, and each loop iteration reaps completed ones (`handle.is_finished()` →
  swap-remove). The live set is bounded by `max_concurrent` (the semaphore caps spawned
  handlers), so retention is O(max_concurrent) — no accumulation. Drain = await remaining
  handles, racing `tokio::time::sleep(drainTimeout)` when set; losers are `.abort()`ed.
- **Multi-isolate composition:** `http_server_serve_multi` already threads ONE shared
  `Arc<Notify>` stop into every isolate's `accept_loop` (`http_server.rs:1550–1552`).
  `srv.shutdown()` on the main isolate fires that same `Notify` → all N isolates stop accepting
  and drain independently (each its own `drainTimeout` window from the shared shutdown instant);
  `serve` resolves when all isolate threads join — the existing join path. `onShutdown` runs on
  the MAIN isolate only (it is a main-side callable; shipping it into isolates would cross the
  airlock — documented).
- **Signals compose for free:** `process.on("SIGTERM", () => srv.shutdown())` — the handler is a
  main-isolate task; `shutdown()` is a sync notify. The init template wires exactly this (§9.3).
- `maxRequests` and `shutdown()` coexist: whichever fires first stops accepting; the drain rules
  are shared (the bounded drain is now just "drain with no timeout").

## 8. cgroup-aware sizing + `os.inContainer()`

### 8.1 The effective-parallelism helper

One new function — `crate::worker::effective_parallelism()` (in `src/worker/pool.rs`, pub for
the server) — replacing the two `num_cpus`-based sites:

```
$ASCRIPT_WORKERS (positive int)            → wins unconditionally (the shipped contract)
else: min(num_cpus::get(), cgroup_cpu_quota().unwrap_or(usize::MAX)).max(1)
```

`cgroup_cpu_quota()` (Linux only; `None` elsewhere and on any read/parse failure — never an
error path):

- **v2:** `/sys/fs/cgroup/cpu.max` — `"max 100000"` → unlimited (`None`); `"200000 100000"` →
  `ceil(quota / period)` = 2. (Read from the process's own cgroup path via
  `/proc/self/cgroup` when the unified hierarchy is mounted elsewhere? **No** — v1 reads the
  standard container-visible path `/sys/fs/cgroup/cpu.max`, which is what a containerized
  process sees in every mainstream runtime; the nested/host-introspection case falls back to
  `num_cpus`, documented best-effort.)
- **v1:** `/sys/fs/cgroup/cpu/cpu.cfs_quota_us` + `cpu.cfs_period_us` — quota `-1` → unlimited;
  else `ceil(quota / period)`. **Both versions covered** (v1 is still what older container hosts
  mount; the cost is one extra file probe on a cold path).
- Consumers: `Pool::new` (`src/worker/pool.rs:59`) and the server's `effective_workers`
  resolution for `workers: 0` (`src/stdlib/http_server.rs:326`) — both become calls to the one
  helper, so pool and acceptor agree by construction. Testability: the reader takes a root-path
  parameter internally (`fn cgroup_cpu_quota_at(root: &Path)`) so unit tests exercise v1/v2/
  malformed/absent fixtures from a temp dir without a real cgroup.
- This changes behavior ONLY inside a quota'd cgroup — where today's answer (host cores) is the
  bug. Outside one, `cgroup_cpu_quota()` is `None` and the result is exactly today's. Stated in
  docs + CLAUDE.md as a deliberate, narrow behavior change.

### 8.2 `os.inContainer() -> bool`

Ungated ambient introspection (an `os` per-func `None` row beside `platform`/`cpuCount` —
asserted in the completeness test). Documented **best-effort heuristic**, in order:
`/.dockerenv` exists (docker) → `/run/.containerenv` exists (podman) → any line of
`/proc/1/cgroup` contains `docker`/`kubepods`/`containerd`/`libpod` (v1 hierarchies; pure-v2
hosts often show `0::/` — the file markers above are the reliable signal there) → `false`.
Non-Linux: the file probes simply miss → `false` (macOS/Windows hosts are not containers).
The docs state plainly: heuristic, no guarantee, intended for sizing/log hints, not security.

## 9. The distribution half

### 9.1 Official base images — **RT-DEPENDENT** (tasks marked, not silently dropped)

The end state: `ghcr.io/ascript-lang/ascript:<ver>` (full toolchain, debian-slim) and
`ascript-rt:<ver>` (runtime-only stub, distroless/static) built `FROM scratch`/distroless using
RT's slim `ascript-rt` stubs. **RT's spec now exists**
(`superpowers/specs/2026-06-12-native-runtime-stubs-design.md` — the stub tier matrix is its §3,
`--oci` its §8) **but is unshipped**, so CNTR ships now what is
real today and marks the rest:

- **Now:** `docker/Dockerfile` (multi-stage: build stage runs `ascript build --native -o app` on
  the shipped 42 MB toolchain binary; runtime stage `debian:bookworm-slim` + the self-contained
  output + a non-root user + `STOPSIGNAL SIGTERM`) — checked in, CI-built against the repo's
  examples, and used by the §9.3 template. This works with today's shipped BNDL `--native`.
- **RT-DEPENDENT (plan tasks explicitly gated):** the scratch/distroless variant and the
  published `ascript-rt` base image — blocked on RT's stub tier matrix and `--oci`. The plan
  carries them as marked tasks with an owner note; merging CNTR without them is sanctioned (a
  documented dependency, not a silent deferral).

### 9.2 Docs — the "Deploying" chapter

New page `docs/content/deploying.md` — NAV entry `['deploying', 'Deploying & containers']` in the
**Introduction** group after `runtime` (the NAV-orphan rule: no NAV entry = unreachable page).
Contents: the Dockerfile walkthrough, SIGTERM→drain wiring, healthchecks, cgroup sizing +
`$ASCRIPT_WORKERS`, `os.inContainer`, capability flags in containers (`--sandbox`,
`--deny`), and the `std/docker` supervisor pattern. Plus: new `docs/content/stdlib/docker.md`
(NAV: `['stdlib/docker', 'Docker (Engine API)']` after `stdlib/net`), and updates to
`stdlib/net.md` (UDS + `{socketPath}`), `stdlib/system.md` (`process.on/off`,
`os.inContainer`), `language/workers.md` (cgroup sizing, signal main-isolate rule, drain in the
multi-core section), `stdlib/caps.md` (dual-cap), `cli.md` (`init`), and `README.md`.

### 9.3 `ascript init --template server`

New CLI subcommand (clap; no `Init` exists today — verified `src/main.rs` command enum):
`ascript init [--template server] [dir]` (default template `server`, default dir `.`; refuses to
overwrite existing files — lists conflicts and exits nonzero; `--force` overwrites). Templates
are `include_str!`-embedded (no network). The `server` template is REAL, runnable code — the
plan carries the full file contents; the shape:

- `main.as` — `server.create()` + routes (`/`, `/healthz` returning `{status:"ok", uptime}`),
  `process.on("SIGTERM"|"SIGINT", () => srv.shutdown())`, `serve({ port: env PORT|8080,
  workers: 0, onShutdown, drainTimeout: 8000 })`, structured `std/log` logging, and resilience
  wired from what is SHIPPED today: `task.retry` with backoff around the outbound-call example
  and `serve`'s `maxConcurrent` as the bulkhead. **RESIL-DEPENDENT (marked task):** swap to
  `std/resilience` circuit-breaker/rate-limit policies when RESIL merges — the template carries
  no placeholder, only working `std/task` code plus a plan-tracked upgrade task.
- `Dockerfile` (the §9.1 multi-stage), `.dockerignore`, `ascript.toml` (name/version +
  commented `[capabilities]` example), `README.md` (run/build/deploy, the SIGTERM window
  explained).
- `init` is CLI-side only (no engine surface) — like `pkg`. Tested end-to-end: scaffold → `check`
  clean → `run` serves → SIGTERM → drains → exits 0.

## 10. Testing

### 10.1 The docker test seam — recorded-fixture mock over UDS

Real-daemon tests cannot gate CI. The seam:

- **`tests/docker.rs`** spawns an in-test **mock Engine daemon**: a tokio `UnixListener` on a
  temp-dir socket serving recorded HTTP/1.1 exchanges — fixtures in `tests/fixtures/docker/`
  (`version.http`, `containers_list.http`, `logs_multiplexed.bin` with real 8-byte frames,
  `events.jsonl`, `pull_progress.jsonl`, `exec_upgrade.bin`, plus malformed/hostile variants:
  truncated frame, over-cap SIZE, bad chunk size line, 64 KiB+ header block). Recorded once from
  a real daemon, checked in, byte-stable. The mock speaks just enough HTTP/1.1 to route by
  request line — it is a test fixture, not a server.
- AScript-side tests run the built binary (the `cap_audit.rs` harness style) with scripts that
  `docker.connect({socketPath: <mock>})` — exercising version negotiation, every unary call, the
  demux (both TTY and multiplexed fixtures), chunked decode, events/pull streaming, exec
  upgrade, and every hostile fixture (Tier-1 err, never a panic/hang — Gate 14).
- **Live tests, opt-in:** `#[ignore]`-free but env-gated — each live test first probes
  `ASCRIPT_DOCKER_LIVE=1` AND the default socket's existence; otherwise it prints a skip note
  and returns (the documented skip-without-docker discipline). Live coverage: ping/version/
  pull(alpine)/create/start/logs/exec/stop/remove round-trip.
- **Examples four-mode tested via the mock:** `examples/docker_info.as` (intro: connect →
  version/info/containers, full Tier-1 handling, socket path from `env.get("DOCKER_SOCK")` with
  the default fallback) and `examples/advanced/docker_supervisor.as` (production-shaped: watch
  `events`, restart exited containers matching a label, stream their logs, SIGTERM-clean
  shutdown). Both are added to `EXAMPLE_SKIPS` (`tests/vm_differential.rs:977`) with reason
  `DaemonDependent` — and instead each gets a dedicated four-mode test in `tests/docker.rs` that
  runs the example under tree-walker / specialized / generic / `.aso` **against the mock socket**
  and asserts byte-identical output (the server_multicore.rs precedent: excluded from the
  blind corpus, four-mode-proven in its own harness).

### 10.2 Cap-audit extension (`tests/cap_audit.rs`)

New rows in the shipped style (`assert_denied`, exact `capability '<cap>' denied` strings):

- `unix.connect`/`unix.listen` denied under `--deny net` AND `--sandbox`.
- **Every `docker.*` entry point denied under `--deny net` AND — separately — under
  `--deny process`** (the conjunction proof: two independent single-deny runs per call), plus
  `--sandbox`, plus in-code `caps.drop("process")` then `docker.connect` (the irreversible
  path). Hermetic: the gate fires at dispatch, before any socket I/O — no daemon needed.
- The per-handle BLOCKER-3 mirror: connect to the MOCK socket, `caps.drop("process")`, then
  `d.ping()` → denied (handle re-check via `governing_caps`).
- Denial-order pin: both denied → `capability 'net' denied` (the `Cap::ALL`-order rule).
- `process.on` denied under `--deny process` / `--sandbox` (it is module-gated, §6.1); the
  positive half: granted runs register + `off` cleanly.
- `os.inContainer` allowed under `--sandbox` (ambient, ungated).

### 10.3 Negative space + gates

`tests/cntr_negative_space.rs`: `ASO_FORMAT_VERSION` still 27; no new `Op`; no new `Value`
variant (`std::mem::size_of` pin + the existing `Value: !Send` assertion untouched); the worker
serializer rejects all four new `NativeKind`s with the field-path panic. Plus the standing
gates: four-mode differential green both configs; clippy both configs; Gate 5 zero `type-*` on
`examples/**`; **Gate 12** — the vm_bench suite re-run same-session A/B proving the `CapReq`
gate change costs nothing on the all-granted path (geomean ≈1.0× vs the pre-branch baseline; the
spec/tw ≥2× floor holds) and the always-armed accept-loop select costs nothing on the server
bench; fmt-idempotent examples; `--no-default-features` builds (caps/`CapReq` are core; `docker`
/`net_unix` are feature-gated out cleanly).

## 11. Implementation surface & cross-cutting checklist

- **New files:** `src/stdlib/net_unix.rs`, `src/stdlib/http1.rs`, `src/stdlib/docker.rs`,
  `tests/docker.rs`, `tests/cntr_negative_space.rs`, `tests/fixtures/docker/*`,
  `templates/server/*` (embedded), `docker/Dockerfile`, `examples/docker_info.as`,
  `examples/advanced/docker_supervisor.as`, `docs/content/deploying.md`,
  `docs/content/stdlib/docker.md`.
- **Modified:** `src/stdlib/mod.rs` (STD_MODULES + routing + `required_cap` → `CapReq` + tests),
  `src/stdlib/caps.rs` (`CapReq`), `src/interp.rs` (gate loop; UDS stage-2 path check),
  `src/value.rs` (4 `NativeKind`s; `governing_caps`), `src/worker/serialize.rs` (non-sendable
  arms), `src/stdlib/net_http.rs` (`{socketPath}` routing), `src/stdlib/process.rs` (signals),
  `src/stdlib/http_server.rs` (drain), `src/stdlib/os.rs` (`inContainer`), `src/worker/pool.rs`
  (`effective_parallelism`), `src/main.rs` (`init`), `Cargo.toml` (`tokio/signal`, `docker`
  feature), `src/check/std_arity.rs` (curated rows for the new fixed-arity fns),
  `src/check/rules/` (workflow-determinism docker/`process.on` coverage), `tests/cap_audit.rs`,
  `tests/vm_differential.rs` (EXAMPLE_SKIPS), `docs/assets/app.js` (NAV ×2), stdlib docs pages,
  `README.md`, `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md` (status flip).
- **Not touched:** grammar/parsers/fmt/LSP (no surface syntax), `.aso`/verify, `det.rs` beyond a
  doc-comment, the GC (no-op `Trace` handles only).

## 12. Scope & rejected alternatives (recorded so they aren't re-litigated)

- **`std/k8s` — PARKED with sketch.** The natural sequel (`k8s.pods/watch/apply` over the
  in-cluster service-account config + the API server). Sketch: the §3.2 http1 client already
  handles the hard part (watch = chunked JSON-lines, the §4.3 events pattern verbatim); auth =
  bearer token + CA from `/var/run/secrets/kubernetes.io/serviceaccount/`; gating = `net` (it is
  not host-root by construction — RBAC scopes it). Deferred: TLS-over-TCP client config is the
  real new surface. Not in CNTR.
- **docker-compose** — out; it is a file-format orchestrator, not an API; `std/docker` +
  AScript code IS the compose story.
- **Windows named pipes for docker** (`npipe://`) — recorded future work; needs a
  `tokio::net::windows::named_pipe` transport arm in http1 + docker; the module's Tier-2
  platform error names it.
- **Rootless-docker nuances** — documented (`socketPath`/`DOCKER_HOST unix://` work today);
  no uid-probing auto-discovery in v1.
- **reqwest-with-UDS-connector / hyperlocal** — rejected (§3.2): no stable reqwest seam, no
  upgrade hijack, larger blast radius than a minimal codec.
- **HTTP keep-alive / connection pool for the docker client** — deferred perf work (control
  plane is low-QPS; correctness first).
- **Interactive exec/attach stdin** — deferred (§4.5); rejected loudly at the arg.
- **Recording signal arrivals as `DetEvent`s** — rejected (§6.4): unfaithful replay is worse
  than a documented exclusion.
- **Per-func ungating of `process.on`** — rejected: uniform module gate is the conservative,
  zero-special-case posture; a `--sandbox`ed top-level program not controlling host signal
  disposition is a feature.
- **Auto-unlinking a stale UDS listen path** — rejected (silent file deletion); the err + the
  caller's explicit `fs.remove` is the policy-honest path.

## 13. Grounding (verified in-repo, 2026-06-12)

- `required_cap` single-cap signature + the gate + `all_granted()` short-circuit:
  `src/stdlib/mod.rs:325, 466–491`; `require_cap` + carve-out stage-2: `src/interp.rs:1119–1205`.
- `Cap::bit`/`CapBits`/`ALL_BITS`: `src/stdlib/caps.rs:36–68, 800–815`; pooled-drop refusal
  pattern: `caps.rs:888`.
- TCP handle substrate to mirror: `src/stdlib/net_tcp.rs` (whole file; `TcpStreamState:31`,
  take/return discipline `:215–235`).
- `ByteStream`/`StreamingBody`/`BodyMode`/SSE reader: `src/stdlib/net_http.rs:102–210`; reqwest
  usage + no-UDS confirmation: doc header `:17–91`, `default_client():364`.
- Hand-rolled HTTP/1 server (NOT hyper) + `accept_loop` + `Notify` stop + lost-wakeup fix +
  bounded drain: `src/stdlib/http_server.rs:4–9, 1276–1421`; multi-isolate stop sharing:
  `:1445–1560`; `ServeOpts`: `:244–254`; `effective_workers`: `:318–326`.
- `for await` native-stream contract + registry: `src/interp.rs:5044–5141, 7713–7732`.
- Worker pool sizing (`$ASCRIPT_WORKERS` → `num_cpus`): `src/worker/pool.rs:57–69`.
- Child-signal names only, no inbound handling: `src/stdlib/process.rs:17–19, 329–341, 805+`.
- tokio features lack `signal` today: `Cargo.toml:32`.
- Cap-audit harness style: `tests/cap_audit.rs` (assert_denied/assert_allowed).
- `EXAMPLE_SKIPS`: `tests/vm_differential.rs:977`; NAV: `docs/assets/app.js:11–62`.
- `ASO_FORMAT_VERSION = 27`: `src/vm/aso.rs:167`. No `init` subcommand exists: `src/main.rs`
  command enum. RT/RESIL specs now exist (`2026-06-12-native-runtime-stubs-design.md`,
  `2026-06-12-resilience-stdlib-design.md`) but are unshipped (dependency-marked tasks §9).
- Docker Engine API facts (HTTP/1.1, versioned `/v1.xx/` paths, `_ping`, multiplexed
  stream header `[type,0,0,0,size_be]`, exec hijack via `Upgrade: tcp`, JSON-lines
  events/pull-progress): the Moby API reference; framing re-verified against recorded daemon
  fixtures during Phase 0 of the plan.
