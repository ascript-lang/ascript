# Container-Native Runtime + `std/docker` (CNTR) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship (1) Unix-domain sockets in `std/net/unix` + a minimal hardened HTTP/1.1 client
behind `std/http`'s `{socketPath}` option; (2) `std/docker` — a typed Engine-API wrapper with
`for await` streams (logs/events/pull) and the 8-byte multiplex demux, gated on **net AND
process** via the `CapReq` chokepoint extension; (3) inbound signals (`process.on/off`);
(4) `server.serve({onShutdown, drainTimeout})` graceful drain + `srv.shutdown()`; (5)
cgroup-aware worker sizing + `os.inContainer()`; (6) the distribution half (Dockerfile, the
Deploying chapter, `ascript init --template server`). Byte-identical across tree-walker /
specialized-VM / generic-VM / `.aso`; **no grammar change, no new `Value` variant, no opcode,
`ASO_FORMAT_VERSION` stays 27**.

**Spec:** `superpowers/specs/2026-06-12-containers-docker-design.md` (CNTR). **Read it first and
in full** — §3 (UDS + the why-not-reqwest decision), §4.4 (the demux protocol — every rule
becomes a test), §5 (the `CapReq` mechanism + denial-order rule), §6.3 (the honest `off()`
semantics), §7.2 (drain mechanics — compose the SRV machinery, don't duplicate), §10 (the mock
seam). Section references (§) below are into it.

**Before writing any code, read these files end to end** (verified 2026-06-12 — **re-grep every
symbol before editing**, names are the anchors):
- `src/stdlib/net_tcp.rs` (the handle pattern `net_unix.rs` mirrors verbatim)
- `src/stdlib/net_http.rs` lines 95–360 (`ByteStream`, `StreamingBody`, `BodyMode`, the SSE
  reader) + `call_http`/`call_http_send` (~`:880+`)
- `src/stdlib/mod.rs` (`required_cap:325`, the gate `:466–491`, `STD_MODULES:240+`, the
  completeness tests `:991+`)
- `src/stdlib/caps.rs` (`Cap::bit`, `CapBits`, `dispatch_decision`, `guard_drop_allowed`)
- `src/interp.rs` (`require_cap:1119`, `check_net_host:1161`, `exec_for_await:5050`,
  `native_stream_method:7713`, `register_resource`/`take_resource`/`return_resource`)
- `src/value.rs` (`NativeKind` + `governing_cap`), `src/worker/serialize.rs` (non-sendable arm)
- `src/stdlib/http_server.rs` (`ServeOpts:244`, `accept_loop:1276` incl. the lost-wakeup
  comment block, `http_server_serve_multi:1445`)
- `src/stdlib/process.rs` (module shape; `signal_name:329`), `src/stdlib/task_mod.rs`
  (`spawn` — the spawn_local + `self.rc()` + `call_value` pattern signals reuse)
- `src/worker/pool.rs` (`Pool::new:58`), `src/stdlib/os.rs`, `src/main.rs` (command enum),
  `tests/cap_audit.rs` (the harness this plan extends), `tests/vm_differential.rs:977`
  (`EXAMPLE_SKIPS`), `docs/assets/app.js:11` (`NAV`)

**Architecture:** Phase 0 preflight pins. Phase 1 (Unit A) — `CapReq` dual-cap chokepoint (FIRST:
everything later keys off it). Phase 2 (Unit B) — `std/net/unix`. Phase 3 (Unit C) — `http1.rs`
codec + `{socketPath}`. Phase 4 (Unit D) — `std/docker` + mock fixture daemon + cap-audit docker
rows. Phase 5 (Unit E) — signals + graceful drain. Phase 6 (Unit F) — cgroup sizing +
`os.inContainer`. Phase 7 (Unit G) — init template, Dockerfile, examples, docs, negative space,
bench, holistic review.

**Tech stack:** Rust; the `!Send` per-isolate runtime (never add `Send` bounds; never hold a
`RefCell` borrow across `.await`); tokio `current_thread` + `LocalSet`; `tokio::net::{UnixStream,
UnixListener}` (`#[cfg(unix)]`); tests via `cargo test` in BOTH feature configs.

**Hard rules carried from the spec:**
- **Zero-cost default (Gate 12):** the `all_granted()` short-circuit is untouched; the `CapReq`
  loop only executes when a cap was dropped. The always-armed accept-loop select must be proven
  cost-free on the server bench. Same-session A/B for both.
- **Denial order:** multi-cap requirements check in `Cap::ALL` order — `net` before `process`;
  pinned by test. Denial strings are the shipped ones, byte-for-byte.
- **Unix-only, loudly:** every UDS/docker entry on non-Unix is the Tier-2
  `Unix-domain sockets are not supported on this platform` (or the per-signal §6.1 message).
  No `#[cfg]`'d-away *routing* — the module arms exist on all platforms.
- **Tier split:** I/O + Engine-API errors = Tier-1 `[value, err]` (with `statusCode` where
  applicable); argument misuse = Tier-2 panic.
- **Untrusted-bytes hardening (Gate 14):** the http1 response parser and the demux are trust
  boundaries — every length capped, every read bounds-checked, malformed input = clean Tier-1
  err, never a panic/hang/allocation bomb. Hostile fixtures are part of the test suite.
- **No daemon in CI:** all docker behavior tests run against the recorded-fixture mock over a
  temp UDS; live tests are env-gated (`ASCRIPT_DOCKER_LIVE=1` + socket probe) and skip loudly.
- New `NativeKind`s: GC-untraced (no-op `Trace`), non-sendable (serializer field-path panic),
  `governing_caps` rows. Caps/`CapReq` are CORE (build under `--no-default-features`); `docker`
  is a default-on feature depending on `net`.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals — the two
sanctioned dependency-marked deferrals are §9.1 (RT base images) and §9.3 (RESIL template
policies), each carried below as an explicit owner-noted task. Branch: `feat/containers-docker`
off `main`. Commit per task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/stdlib/net_unix.rs`, `src/stdlib/http1.rs`, `src/stdlib/docker.rs`
- `tests/docker.rs`, `tests/cntr_negative_space.rs`, `tests/fixtures/docker/*`
- `templates/server/{main.as,Dockerfile,.dockerignore,ascript.toml,README.md}` (embedded via
  `include_str!`), `docker/Dockerfile`
- `examples/docker_info.as`, `examples/advanced/docker_supervisor.as`
- `docs/content/deploying.md`, `docs/content/stdlib/docker.md`
- `bench/CNTR_RESULTS.md`

**Modified files:** `src/stdlib/{mod,caps,net_http,process,http_server,os}.rs`, `src/interp.rs`,
`src/value.rs`, `src/worker/{serialize,pool}.rs`, `src/main.rs`, `Cargo.toml`,
`src/check/std_arity.rs`, the workflow-determinism rule file, `tests/cap_audit.rs`,
`tests/vm_differential.rs`, `docs/assets/app.js`, `docs/content/stdlib/{net,system,caps}.md`,
`docs/content/{cli,language/workers}.md`, `README.md`, `CLAUDE.md`, `superpowers/roadmap.md`,
`goal-perf.md`.

---

## Phase 0 — Preflight: branch + semantic pins

### Task 0.1: branch + pin the inherited behavior CNTR composes

**Files:** test additions only.

- [ ] **Step 1:** `git checkout -b feat/containers-docker main`. `cargo build --release` clean.
  Record the same-session Gate-12 BASELINE now: run the vm_bench suite + the server bench and
  save the numbers into branch notes (they are the A side of the Task 7.6 A/B).
- [ ] **Step 2:** PASSING pin tests (if any fails, STOP — ground truth moved):
  - `src/stdlib/mod.rs` tests: `required_cap("net_tcp","connect") == Some(Cap::Net)` and
    `required_cap("process","spawn") == Some(Cap::Process)` *(these get mechanically migrated in
    Task 1.1 — pinning first proves the migration preserved verdicts)*.
  - `src/interp.rs` test: `native_stream_method(NativeKind::SseStream) == Some("next")`.
  - `tests/cntr_negative_space.rs` (created now): `ASO_FORMAT_VERSION == 27`.
- [ ] **Step 3:** `cargo test` green BOTH configs. Commit —
  `test(cntr): phase-0 pins (required_cap verdicts, stream registry, ASO 27)`.

### Task 0.2: Phase 0 review

- [ ] Independent reviewer: pins run green on both configs; baseline bench numbers recorded;
  no non-test source changed.

---

## Phase 1 — Unit A: the `CapReq` dual-cap chokepoint (spec §5)

### Task 1.1: `CapReq` + `required_cap` migration (failing test first)

**Files:** `src/stdlib/caps.rs`, `src/stdlib/mod.rs`, `src/interp.rs`.

- [ ] **Step 1 (failing tests):** in `src/stdlib/mod.rs` tests:

```rust
#[test]
fn required_cap_docker_requires_both_net_and_process() {
    use caps::{Cap, CapReq};
    let req = required_cap("docker", "anything");
    let caps: Vec<Cap> = req.iter().collect();
    assert_eq!(caps, vec![Cap::Net, Cap::Process], "docker = net AND process, Cap::ALL order");
    // Single-cap rows keep their exact verdicts (the Phase-0 pins, restated post-migration).
    assert_eq!(required_cap("fs", "readFile").iter().collect::<Vec<_>>(), vec![Cap::Fs]);
    assert_eq!(required_cap("net_unix", "connect").iter().collect::<Vec<_>>(), vec![Cap::Net]);
    assert!(required_cap("math", "abs").is_empty());
}
```

  And in `src/stdlib/caps.rs` tests: `CapReq::one(Cap::Net).and(Cap::Process).iter()` order;
  `CapReq::NONE.is_empty()`; `CapReq` is `Copy` + `Send`.
- [ ] **Step 2:** add `CapReq` to `caps.rs` exactly as spec §5.2 (the `Copy(u8)` newtype with
  `NONE`/`one`/`and`/`is_empty`/`iter` in `Cap::ALL` order). Requires `Cap::bit` stays private —
  give `CapReq` module-internal access (same file).
- [ ] **Step 3:** migrate `required_cap` to return `CapReq` (every arm `CapReq::one(..)`;
  fallthrough `CapReq::NONE`; add `"net_unix" => CapReq::one(Cap::Net)` and
  `"docker" => CapReq::one(Cap::Net).and(Cap::Process)` — `docker` arm `#[cfg(feature =
  "docker")]`, `net_unix` under `net`, matching how `sqlite`/`postgres` arms are cfg'd). Update
  the gate in `call_stdlib`:

```rust
if !cap_bits.all_granted() {
    // CNTR §5.2: a requirement may be a CONJUNCTION (docker = net ∧ process).
    // Checked in Cap::ALL order → the first denied cap names the error (pinned).
    for cap in required_cap(module, func).iter() {
        self.require_cap(cap, module, func, args, span)?;
    }
    if module == "fs" { /* unchanged stage-2 block */ }
}
```

- [ ] **Step 4:** mechanically migrate the completeness tests (`required_cap_complete_enumeration`,
  the every-module-classified sweep — `is_empty()` is the new "ungated" probe) and any other
  `required_cap` callers (grep ALL of them). Phase-0 pins updated to the `CapReq` form in the
  same commit (they exist to prove verdict preservation).
- [ ] **Step 5:** `cargo test` + clippy BOTH configs green (`CapReq` is core — verify
  `--no-default-features`). Commit — `feat(caps): CapReq conjunction requirements at the
  chokepoint (CNTR §5.2) — docker preregistered net∧process`.

### Task 1.2: `governing_caps` per-handle re-check + denial-order audit rows

**Files:** `src/value.rs`, `src/interp.rs` (`call_native_method` re-check site),
`tests/cap_audit.rs`.

- [ ] **Step 1 (failing test):** unit test in `value.rs`:
  `NativeKind::TcpStream.governing_caps().iter().collect::<Vec<_>>() == vec![Cap::Net]`, and an
  exhaustiveness-style assertion that every kind previously returning `Some`/`None` maps to a
  nonempty/empty `CapReq` (grep the old method's arms; verdicts preserved one-for-one).
- [ ] **Step 2:** rename/migrate `governing_cap() -> Option<Cap>` to
  `governing_caps() -> CapReq` (single method; every existing arm `CapReq::one`; ungated kinds
  `CapReq::NONE`). Update the `call_native_method` re-check to the same `for cap in …iter()`
  loop, behind the same `all_granted()` short-circuit it has today.
- [ ] **Step 3:** cap-audit denial-order pin (`tests/cap_audit.rs`): a script calling a
  to-be-`docker` path can't exist yet — instead pin the ORDER mechanism with a caps-level unit
  test (`CapReq::one(Cap::Net).and(Cap::Process)` iterates net-first) and leave the end-to-end
  docker rows to Task 4.6 (they need the module). Note this forward link in a comment.
- [ ] **Step 4:** full suite + clippy both configs. Commit —
  `feat(caps): governing_caps CapReq per-handle re-check (CNTR §5.3)`.

### Task 1.3: Phase 1 review

- [ ] Independent reviewer runs: both configs build/test/clippy; greps for any remaining
  `Option<Cap>` returns; **adversarial probe:** `--deny net` still denies `net.lookup` with the
  exact shipped string (run the existing cap_audit suite); confirms the gate loop is inside the
  `!all_granted()` branch (read the diff — Gate-12); confirms no behavior change for any
  existing module (cap_audit 100% green).
- [ ] Holistic phase review: the `CapReq` design matches spec §5.2 exactly; no second lookup
  table snuck in; completeness sweep still forces classification of every module.

---

## Phase 2 — Unit B: `std/net/unix` (spec §3.1)

### Task 2.1: the module — connect/listen + stream methods (failing test first)

**Files:** new `src/stdlib/net_unix.rs`; `src/stdlib/mod.rs` (STD_MODULES + `pub mod` +
both routing arms, under `#[cfg(feature = "net")]`); `src/value.rs` (2 `NativeKind`s);
`src/worker/serialize.rs`.

- [ ] **Step 1 (failing test):** in `net_unix.rs` tests (the `net_tcp.rs` echo-peer fixture
  pattern, over a temp-dir socket path):

```rust
#[cfg(unix)]
#[tokio::test]
async fn connect_write_readline_against_uds_echo_peer() {
    let path = uds_temp_path("echo");             // temp dir + unique name, cleaned up
    let _peer = spawn_uds_echo_peer(&path).await; // UnixListener; accepts one; echoes
    let src = format!(r#"
import * as unix from "std/net/unix"
let [stream, err] = await unix.connect("{path}")
print(err)
await stream.write("hello\n")
print(await stream.readLine())
stream.close()
"#);
    assert_eq!(run(&src).await, "nil\nhello\n");
}
```

- [ ] **Step 2:** implement as the structural mirror of `net_tcp.rs` (spec §3.1): a
  `UnixStreamState` over `BufReader<tokio::net::UnixStream>` with `read_upto` (the `take(n)`
  no-zero-fill discipline copied), `read_line_bytes`, `read_to_end_bytes`, `write_all`;
  `connect(path)`/`listen(path)` Tier-1 entries; method dispatch `read/readLine/readToEnd/
  write/close` + `accept/close` with the exact take-out-across-await + EOF-finalize +
  `read(0)`-no-finalize rules (each rule gets the mirrored test). Listener wrapper struct whose
  `Drop` best-effort-unlinks the path it bound (spec §3.1 — test: socket file gone after
  `close()`).
- [ ] **Step 3:** `NativeKind::{UnixStream, UnixListener}` — every exhaustive match: `type_name`
  (`"unixStream"`/`"unixListener"`), `governing_caps` (`CapReq::one(Cap::Net)`), the serializer
  non-sendable arm (test: passing a stream to a `worker fn` raises the field-path panic), no-op
  `Trace` (it is a Native — no trace arm to write; assert in the negative-space test).
- [ ] **Step 4:** non-Unix arms: `#[cfg(not(unix))]` bodies for `connect`/`listen` raising
  Tier-2 `Unix-domain sockets are not supported on this platform` (compile-checked via
  `cargo check --target x86_64-pc-windows-msvc` if the toolchain is present; otherwise a
  `#[cfg(not(unix))]` unit test stub + reviewer note — do NOT skip the arm).
- [ ] **Step 5:** edge tests: connect to a nonexistent path → Tier-1 err; listen on an existing
  path → Tier-1 `EADDRINUSE`-class err (no unlink — assert the file survives); read-after-EOF →
  nil repeatedly; resource_count reclaimed after close (the net_tcp test set, mirrored).
- [ ] **Step 6:** UDS stage-2 carve-out (spec §3.1): `Interp::check_unix_path(path, span)` —
  `net_scope: None` → immediate `Ok` (Gate-12); `Some(scope)` → allowed iff `unix:<path>` (the
  literal, after the same `canonical_lossy` best-effort canonicalization the fs scope uses) is
  on the allow list, else `capability 'net' denied for unix socket '<path>'`. Called from
  `connect`/`listen`. Tests: carve-out `--deny-net`-style scope blocks UDS; an
  `allow:["unix:<path>"]` carve-out admits it.
- [ ] **Step 7:** `std_arity.rs` rows (`("std/net/unix","connect") => 1`, `("std/net/unix",
  "listen") => 1`) — the drift-guard test cross-checks exports. cap_audit rows:
  `unix.connect`/`unix.listen` denied under `--deny net` AND `--sandbox` (`#[cfg(all(unix,
  feature = "net"))]`).
- [ ] **Step 8:** full suite + clippy both configs. Commit —
  `feat(net): std/net/unix — UDS connect/listen handles, net-gated (CNTR §3.1)`.

### Task 2.2: Phase 2 review

- [ ] Independent reviewer: runs the UDS tests; probes edges — long line reads, a peer that
  closes mid-write, `listen` path >107 bytes (the `sun_path` limit — must be a clean Tier-1
  err, test it), double-close idempotence; verifies the four-mode differential is untouched
  (no examples yet) and `--no-default-features` builds (module is `net`-gated).

---

## Phase 3 — Unit C: the http1 codec + `{socketPath}` (spec §3.2)

### Task 3.1: `src/stdlib/http1.rs` — request writer + response parser (failing tests first)

**Files:** new `src/stdlib/http1.rs` (`#[cfg(all(unix, feature = "net"))]` internals; the
module itself under `net`).

- [ ] **Step 1 (failing tests — the codec is pure enough for direct unit tests):** drive the
  parser over in-memory duplex streams (`tokio::io::duplex`), covering:
  - simple 200 + `Content-Length` body;
  - `Transfer-Encoding: chunked` (multi-chunk, hex sizes incl. uppercase + chunk extensions
    `;ext=1` tolerated-and-ignored, terminal `0\r\n\r\n`, trailer headers skipped);
  - `Connection: close` read-to-EOF body;
  - 204 empty body; HEAD-style no-body;
  - **hostile set (each → clean Tier-1 err, never panic/hang):** header block > 64 KiB; > 256
    headers; non-numeric / overflowing Content-Length; chunk size > 16 MiB cap; bad hex; missing
    CRLF after a chunk; truncated mid-head and mid-body; status line garbage; status < 100 or
    > 599;
  - **upgrade:** a `101 Switching Protocols` head followed by raw bytes — the parser returns the
    head + hands back the transport + the exact leftover bytes.
- [ ] **Step 2:** implement:

```rust
/// CNTR §3.2: a minimal, hardened HTTP/1.1 CLIENT codec for {socketPath} requests
/// only (TCP requests keep reqwest). Generic over the transport so unit tests use
/// an in-memory duplex and docker uses UnixStream.
pub(crate) struct Http1Response<T> {
    pub status: u16,
    pub headers: Vec<(String, String)>,   // order-preserving; lookup is ASCII-case-insensitive
    pub body: Http1Body<T>,
}
pub(crate) enum Http1Body<T> {
    /// Framed body, consumable as net_http's ByteStream (chunked or content-length
    /// or read-to-EOF — the decoder is a Stream adapter over the transport).
    Stream(/* BufReader<T> + framing state */ ...),
    /// 101/upgrade: the raw transport + bytes already buffered past the head.
    Upgraded { transport: T, leftover: Vec<u8> },
}

pub(crate) async fn send_request<T: AsyncRead + AsyncWrite + Unpin>(
    io: T, req: &Http1Request<'_>,
) -> Result<Http1Response<T>, String>   // String err → caller wraps as Tier-1
```

  Request writer per spec §3.2 (request line, `Host: localhost`, `Connection: close` default —
  overridden to `Upgrade` by the caller for hijack — `Content-Length` body). The body decoder
  exposes `into_byte_stream(self) -> ByteStream` producing `net_http`'s exact
  `Pin<Box<dyn Stream<Item = io::Result<Bytes>>>>` type so `StreamingBody`/`BodyMode` are reused
  verbatim, and `read_to_end(self, cap) -> Result<Vec<u8>, String>` for the buffered path
  (cap = the stdlib `MAX_ALLOC_COUNT` discipline).
- [ ] **Step 3:** clippy + tests both configs. Commit —
  `feat(net): http1 — minimal hardened HTTP/1.1 client codec (CNTR §3.2)`.

### Task 3.2: `{socketPath}` routing in `std/http`

**Files:** `src/stdlib/net_http.rs` (+ a `ResourceState` arm for the UDS response if buffered
responses are handle-backed on the reqwest path — mirror whatever shape `call_http_send`
returns today; the script-visible surface must be IDENTICAL).

- [ ] **Step 1 (failing test):** spawn an in-test UDS HTTP/1.1 server (tokio UnixListener +
  hand-written response bytes — three routes: `/text` 200 text, `/json` 200 JSON, `/chunked`
  chunked), then:

```rust
#[cfg(unix)]
#[tokio::test]
async fn http_request_over_socket_path_matches_tcp_surface() {
    let sock = spawn_uds_http_fixture().await;
    let src = format!(r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/json", method: "GET" }})
print(err)
print(resp.status)
let [v, jerr] = await resp.json()
print(v.ok)
"#);
    assert_eq!(run(&src).await, "nil\n200\ntrue\n");
}
```

  Plus: `opts.stream: true` over `/chunked` → `resp.body.read()` chunks then nil (the
  `StreamingBody` reuse proof); `socketPath` + `body:{stream:…}` → the spec's Tier-2 clear
  error; non-Unix → the platform Tier-2 (cfg'd test).
- [ ] **Step 2:** in `call_http_send` (or a sibling `call_http_send_uds` it routes to when
  `opts.socketPath` is present): connect `UnixStream`, build the `Http1Request` from
  method/path/headers/body (canonical form `request({socketPath, path, …})`; verb helpers
  accept `socketPath` with the URL's path extracted, host ignored — spec §3.2), check the §3.1
  UDS stage-2 carve-out, send, and adapt: buffered → the same response shape the reqwest path
  registers; `stream:true` → `StreamingBody::…` over `into_byte_stream()` with the same
  `HttpBody` kind + `BodyMode` opts. Timeouts: apply `opts.timeout` as a whole-request
  `tokio::time::timeout` (document: no separate connect/read split on the UDS path).
- [ ] **Step 3:** surface-parity test battery: status/headers/text/json/typed `json(Class)` /
  stream — each asserted equal in SHAPE to a same-fixture TCP request through the reqwest path
  (run the same script against the hyper TCP fixture `net_http.rs` already has, diff the
  printed output).
- [ ] **Step 4:** full suite + clippy both configs. Commit —
  `feat(net): std/http {socketPath} over the http1 UDS client (CNTR §3.2)`.

### Task 3.3: Phase 3 review

- [ ] Independent reviewer: runs the hostile-fixture battery; **probes:** a fixture that sends
  the head then stalls forever (timeout opt must fire, no hang); a chunked body whose declared
  chunk exceeds the cap; double-`resp.text()` (second call errors cleanly like the reqwest
  path); confirms `ByteStream` type reuse (no parallel streaming body type was introduced —
  grep). Holistic phase review of Units A–C combined.

---

## Phase 4 — Unit D: `std/docker` (spec §4) + the mock seam (spec §10.1)

### Task 4.1: fixtures + the mock Engine daemon (test infrastructure first)

**Files:** `tests/fixtures/docker/*`, the mock helper inside `src/stdlib/docker.rs` `#[cfg(test)]`
AND a binary-test copy in `tests/docker.rs` (a small shared `mod` via `include!` is acceptable;
do not duplicate logic — pick one home and re-export).

- [ ] **Step 1:** write the fixture files (recorded from a real daemon where available, else
  hand-assembled to the protocol — they are byte-exact HTTP/1.1):
  `version.http` (negotiation: `ApiVersion: "1.43"`), `version_old.http` (`1.20` → floor
  rejection), `ping.http`, `containers_list.http`, `inspect.http`, `create.http` (201 + Id),
  `start_204.http`, `stop_204.http`, `wait.http` (`{"StatusCode":0}`), `remove_204.http`,
  `images_list.http`, `image_remove.http`, `error_404.http` (`{"message":"No such container"}`),
  `logs_multiplexed.bin` (real 8-byte frames: stdout "hello\n", stderr "oops\n"),
  `logs_tty.bin` (raw text, no frames), `events.jsonl.http` (chunked, 3 events),
  `pull_progress.http` (chunked JSON-lines incl. a final success status),
  `pull_error.http` (an in-stream `{"error": …}` line),
  `exec_create.http`, `exec_upgrade.bin` (101 head + multiplexed frames), `exec_inspect.http`,
  hostile: `logs_truncated_frame.bin`, `logs_oversize_frame.bin` (SIZE > 16 MiB),
  `chunked_bad_size.http`.
- [ ] **Step 2:** the mock daemon: a tokio task on a temp-path `UnixListener`; per connection,
  read the request head, route by `METHOD /v*/path` prefix to a fixture, write its bytes, close
  (or for `follow`/`events`/upgrade fixtures: write incrementally with small sleeps to exercise
  pull-driven reads). Returns the socket path + a shutdown guard.
- [ ] **Step 3:** a smoke test: raw `unix.connect` to the mock + hand-written `GET /_ping` →
  `OK` (proves the mock independent of the docker module). Commit —
  `test(docker): recorded-fixture mock Engine daemon over UDS (CNTR §10.1)`.

### Task 4.2: `docker.connect` + version negotiation + unary calls

**Files:** new `src/stdlib/docker.rs`; `Cargo.toml` (`docker = ["net"]` in `[features]`, added
to `default`); `src/stdlib/mod.rs` (STD_MODULES `"std/docker"`, `pub mod`, routing arm —
`required_cap` arm already landed in Task 1.1; flip its cfg on); `src/value.rs`
(`NativeKind::DockerClient`).

- [ ] **Step 1 (failing tests):** against the mock:

```rust
#[cfg(unix)]
#[tokio::test]
async fn connect_negotiates_version_and_lists_containers() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
print(d.apiVersion)
let [cs, e2] = await d.containers({{ all: true }})
print(len(cs))
print(cs[0].Names[0])
"#);
    assert_eq!(run(&src).await, "nil\n1.43\n2\n/web\n");
}
```

  Plus: old daemon → Tier-1 floor err; unreachable socket → Tier-1; 404 inspect →
  `err.statusCode == 404` + the daemon's message; `tcp://` DOCKER_HOST → the spec's Tier-1;
  204 calls → `[nil, nil]`; misuse (`d.inspect(42)`) → Tier-2.
- [ ] **Step 2:** implement (spec §4.1/§4.2): socket resolution order (opts → `$DOCKER_HOST`
  `unix://` → `/var/run/docker.sock`); negotiation `GET /v1.24/version` → clamp to
  `[1.24, 1.43]`; `ResourceState::DockerClient { socket_path, api_version }`; one fresh
  `UnixStream` + `http1::send_request` per call; the full unary table (`ping`, `version`,
  `info`, `containers`, `inspect`, `create`, `start`, `stop`, `restart`, `remove`, `wait`,
  `images`, `removeImage`, `close`) with query-param encoding (`filters` JSON-encoded via the
  `json` module's encoder — reuse, don't re-write) and the non-2xx → `{message, statusCode}`
  err mapping. Handle fields: `apiVersion`, `socketPath`. Take-out-across-await for the client
  state; method dispatch via `call_native_method` like the TCP handles.
- [ ] **Step 3:** non-Unix Tier-2 arms; std_arity rows for the fixed-arity fns (`connect` is
  0-required — omit; `inspect/start/wait/removeImage…` = 1); serializer non-sendable arm +
  test; `governing_caps` for `DockerClient` = `CapReq::one(Cap::Net).and(Cap::Process)`.
- [ ] **Step 4:** full suite + clippy both configs (incl. `--no-default-features` — docker
  compiled out cleanly, STD_MODULES entry stays per the feature-independent rule like
  `std/ffi`'s). Commit — `feat(docker): client + version negotiation + container/image unary
  API over UDS (CNTR §4.1–4.2)`.

### Task 4.3: the demux + `logs`/`events`/`pull` streams

**Files:** `src/stdlib/docker.rs`, `src/value.rs` (`NativeKind::DockerStream`),
`src/interp.rs` (`native_stream_method` arm).

- [ ] **Step 1 (failing tests):**
  - demux unit tests straight over fixture bytes: multiplexed → `[{stream:"stdout",
    text:"hello\n"}, {stream:"stderr", text:"oops\n"}]`; TTY auto-detect → one stdout item;
    truncated mid-header/mid-payload → Tier-1 err item; oversize SIZE → Tier-1, no allocation;
    frame split across reads reassembles (feed 1 byte at a time).
  - end-to-end `for await`:

```rust
#[cfg(unix)]
#[tokio::test]
async fn logs_stream_for_await_demuxes_stdout_stderr() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, err] = await d.logs("abc", {{ stdout: true, stderr: true }})
print(err)
for await (entry in logs) {{ print("${{entry.stream}}:${{entry.text}}") }}
"#);
    assert_eq!(run(&src).await, "nil\nstdout:hello\n\nstderr:oops\n\n");
}
```

  - `events` yields decoded objects, `break` + `close()` reclaims the fd (resource_count test);
    `pull` progress objects then end; `pull_error` fixture → terminal `[nil, err]` then end.
- [ ] **Step 2:** implement: `ResourceState::DockerStream` holding the framing state over the
  http1 `ByteStream`/buffered reader + a `StreamFraming` enum (`Multiplexed`-or-`Tty` resolved
  on the first 8 bytes per spec §4.4; `JsonLines` for events/pull). `next()` returns the
  `[item, err]` / `[nil, nil]`-end contract; `close()` takes the resource. Register
  `DockerStream => Some("next")` in `native_stream_method` (spec §4.3 — this alone makes
  `for await` work on BOTH engines; add a VM-path test via `vm_run_source` to prove it).
  `governing_caps` = net∧process; serializer non-sendable arm + test.
- [ ] **Step 3:** full suite + clippy both configs. Commit —
  `feat(docker): logs/events/pull as for-await streams + the 8-byte multiplex demux (CNTR §4.3–4.4)`.

### Task 4.4: exec (create/start-with-hijack/inspect + the convenience composition)

**Files:** `src/stdlib/docker.rs`.

- [ ] **Step 1 (failing tests):** against the upgrade fixture: `d.execCreate` returns the id;
  `d.execStart` yields a demuxed `DockerStream`; `d.execInspect` returns the object;
  `d.exec(id, {cmd})` returns `{exitCode, stdout, stderr}` assembled from the fixtures;
  `attachStdin: true` → the spec's Tier-2 deferral error (exact message pinned).
- [ ] **Step 2:** implement over the http1 `Upgraded { transport, leftover }` arm: build the
  demux reader from `leftover` + the raw `UnixStream`. `exec` = create → start → drain →
  inspect, mirroring `process.run`'s result shape (spec §4.5).
- [ ] **Step 3:** suite + clippy. Commit — `feat(docker): exec create/start(hijack)/inspect +
  d.exec convenience (CNTR §4.5)`.

### Task 4.5: live tests (env-gated) + workflow-determinism coverage

**Files:** `tests/docker.rs`, the workflow-determinism rule file (grep
`src/check/rules/` for its module).

- [ ] **Step 1:** live round-trip test gated on `ASCRIPT_DOCKER_LIVE=1` + socket existence
  (else `eprintln!` skip note + return — the documented skip-without-docker discipline):
  ping → pull alpine → create (`["sh","-c","echo hi; echo err >&2"]`) → start → wait → logs
  (assert both demux streams) → exec → remove. Run it once locally; record the result in the
  task commit message.
- [ ] **Step 2:** workflow-determinism: verify (rule test, failing first if uncovered) that a
  `docker.*` call inside a workflow body is flagged; extend the rule's nondeterministic-module
  classification with `docker` if list-based. 0 FP on `examples/**` (Gate 5 sweep).
- [ ] **Step 3:** suite + clippy. Commit — `test(docker): env-gated live round-trip +
  workflow-determinism coverage (CNTR §4.6, §10.1)`.

### Task 4.6: cap-audit docker rows (the §10.2 conjunction proof)

**Files:** `tests/cap_audit.rs`.

- [ ] **Step 1:** in the shipped `assert_denied` style, `#[cfg(all(unix, feature = "docker"))]`:
  - `docker.connect()` denied under `--deny net` → `capability 'net' denied`; under
    `--deny process` → `capability 'process' denied`; under `--sandbox`; via in-code
    `caps.drop("process")`. (Hermetic — the gate fires at dispatch, before any socket I/O.)
  - Denial order: under `--deny net --deny process` → `capability 'net' denied` (Cap::ALL order).
  - BLOCKER-3 mirror: connect to the MOCK socket (start it inside the test, pass the path via a
    temp script), `caps.drop("process")`, then `d.ping()` → denied (per-handle
    `governing_caps`). Same for an open `d.logs` stream's `next()`.
  - `unix.connect` rows already landed (Task 2.1) — verify still green.
  - `process.on("SIGTERM", …)` denied under `--deny process` and `--sandbox` (forward row —
    `#[cfg]`-gate it on Phase 5 landing or place it in Task 5.1; reviewer ensures no orphan).
  - Positive half: with no denials, `docker.connect` against the mock succeeds.
- [ ] **Step 2:** suite green. Commit — `test(caps): cap-audit — every docker.* denied under
  --deny net AND --deny process; order + per-handle re-check pinned (CNTR §10.2)`.

### Task 4.7: Phase 4 review

- [ ] Independent reviewer: runs `tests/docker.rs` + cap_audit; **probes:** kill the mock
  mid-stream (Tier-1, no hang); a fixture with `Content-Length` lying (longer than body);
  `for await` + `break` then reuse the stream handle (`next()` after close → end, not panic);
  `d.close()` then `d.ping()` (clean err); confirms NO real daemon was needed for any
  non-live test (unset `DOCKER_HOST`, run suite).
- [ ] Holistic phase review: API surface vs spec §4 table 1:1; demux rules vs §4.4 1:1; the
  Tier split consistent across every call.

---

## Phase 5 — Unit E: inbound signals + graceful drain (spec §6–§7)

### Task 5.1: `process.on` / `process.off`

**Files:** `Cargo.toml` (tokio `signal` feature), `src/stdlib/process.rs`, `src/interp.rs`
(signal-handler registry on `Interp` — a `RefCell<HashMap<&'static str, SignalReg>>`),
`tests/cli.rs` or a new `tests/signals.rs` (spawned-binary tests can send real signals).

- [ ] **Step 1 (failing test):** integration test (Unix): spawn the built binary running a
  script that registers `process.on("SIGTERM", …)` printing then exiting via a flag; send
  SIGTERM via `kill`; assert output + exit 0. Plus: unregistered SIGTERM still kills (exit
  143/signal); `off()` then SIGTERM → exit 143 (the §6.3 emulated restore); SIGINT handler;
  unknown name / SIGKILL → Tier-2 (in-process `run_source` tests); handler receives the
  signal-name arg; last-wins replacement.
- [ ] **Step 2:** implement per spec §6.1: name table → `SignalKind` (term/interrupt/hangup/
  quit/user_defined1/user_defined2; Windows: only `"SIGINT"` via `ctrl_c`, others Tier-2);
  first `on` for a signal spawns ONE listener task (`spawn_local` with the `self.rc()` weak
  pattern — copy `task.spawn`'s exact spawn shape from `task_mod.rs`) looping
  `signal.recv().await` → read the current handler from the registry (clone the `Value` out,
  drop the borrow, then `call_value(handler, vec![Value::Str(name)], span)`; a `Control::Panic`
  is reported like a panicking spawned task, loop continues); `on` again = registry swap;
  `off` = registry entry → `Restored` state: the loop's next receipt prints nothing and exits
  the process with `128 + signo` (flush output first — reuse the runtime's normal exit path).
  Tier-2 refusal in a worker isolate (the `caps_drop_allowed`-style isolate flag — reuse that
  exact plumbing; test via a `worker fn` calling `process.on`).
- [ ] **Step 3:** determinism: `det.rs` doc-comment naming the exclusion;
  workflow-determinism rule flags `process.on` in a workflow body (failing-first rule test).
- [ ] **Step 4:** cap-audit `process.on` rows (Task 4.6 forward link closed). Suite + clippy
  both configs (signal registry is `sys`-gated with `std/process`). Commit —
  `feat(process): inbound signal handlers — process.on/off via tokio::signal, main-isolate
  only (CNTR §6)`.

### Task 5.2: `srv.shutdown()` + `serve({onShutdown, drainTimeout})`

**Files:** `src/stdlib/http_server.rs`, `tests/server_multicore.rs` (multi-isolate drain test).

- [ ] **Step 1 (failing tests):**
  - single-isolate: serve with a slow handler (`time.sleep(300)`), fire one request, call
    `srv.shutdown()` from a spawned task mid-request → serve resolves AFTER the response is
    written (client got 200 — the drain proof); `onShutdown` printed exactly once, before the
    drain wait.
  - `drainTimeout: 50` with a 10s handler → serve resolves ~50ms after shutdown; the client
    sees a closed connection; the abort count `warn`-logged.
  - `shutdown()` before `serve` → serve returns immediately (after `onShutdown`).
  - unchanged-behavior pin: `serve({maxRequests: 1})` output byte-identical to today (run the
    existing server tests — they ARE this pin).
  - multi-isolate: `workers: 2` + shutdown → all isolates stop, serve resolves, total served ==
    requests completed (the existing budget test style).
  - composition: `process.on("SIGTERM", () => srv.shutdown())` end-to-end in a spawned-binary
    test — send SIGTERM, assert in-flight request completed + exit 0.
- [ ] **Step 2:** implement per spec §7.2, composing not duplicating:
  - `ServeOpts` gains `on_shutdown: Option<Value>`, `drain_timeout_ms: Option<u64>` (parse
    beside the existing fields).
  - The server resource gains a `shutdown: Arc<tokio::sync::Notify>` + `shutdown_armed:
    Arc<AtomicBool>` created at handle creation; `srv.shutdown()` (new handle method) sets the
    flag + `notify_waiters()` — sync, idempotent.
  - `accept_loop`: the select guard generalizes `bounded` → `bounded || stoppable` (stoppable =
    the handle's notify was threaded in — always, for serve). Reuse the EXACT
    register-`enable()`-recheck sequence (the lost-wakeup block at `:1311–1332`), rechecking
    `budget == 0 || shutdown_armed`. In-flight handles retained always + reaped per iteration
    (`is_finished()` swap-remove — bounded by `max_concurrent`); on stop: run `on_shutdown`
    (main side, once — guard with a `Once`-style flag shared across isolates' main-side caller,
    NOT inside accept_loop for multi), then drain = await remaining handles raced against the
    optional `drain_timeout` sleep, `.abort()` losers, `warn` the count.
  - Multi-isolate: thread the handle's `shutdown` Notify as (or fused with) the existing shared
    `stop` (`http_server_serve_multi:1550` — one `Notify` reaches every isolate already; fuse by
    using the handle's Notify AS the loop stop and keeping the budget-exhaustion path notifying
    it too). `drain_timeout` passes into each isolate's loop args.
- [ ] **Step 3:** suite + clippy both configs; the full existing server test battery green
  unchanged (the behavioral-identity proof for the always-armed select). Commit —
  `feat(server): graceful drain — srv.shutdown() + serve({onShutdown, drainTimeout}),
  multi-isolate composed (CNTR §7)`.

### Task 5.3: Phase 5 review

- [ ] Independent reviewer: runs the signal + drain suites repeatedly (10×) for flake; probes:
  SIGTERM twice (second during drain — no double-`onShutdown`, no panic); `shutdown()` from
  inside a request handler; `off` for a never-registered signal (clean Tier-2); reads the
  accept_loop diff against the lost-wakeup invariant (the register-enable-recheck order MUST be
  preserved — this is the SRV bug class, named in the review brief).
- [ ] Holistic phase review of Unit E.

---

## Phase 6 — Unit F: cgroup-aware sizing + `os.inContainer()` (spec §8)

### Task 6.1: `effective_parallelism` + the cgroup reader

**Files:** `src/worker/pool.rs` (the helper, pub), `src/stdlib/http_server.rs`
(`effective_workers` → the helper), `src/stdlib/os.rs` (`inContainer`),
`src/stdlib/mod.rs` (completeness-test row: `os.inContainer` ungated).

- [ ] **Step 1 (failing tests):** unit tests over fixture roots (temp dirs):
  - v2 `cpu.max` = `"200000 100000"` → quota 2; `"max 100000"` → None; `"150000 100000"` →
    ceil → 2; malformed/absent → None.
  - v1 `cpu/cpu.cfs_quota_us` = `400000` + period `100000` → 4; quota `-1` → None.
  - precedence: `$ASCRIPT_WORKERS=3` beats everything (env-var test with the existing
    pool-test serialization discipline — check how pool tests isolate env, mirror it);
    no env + quota 2 + 8 cpus → 2; no quota → `num_cpus`.
  - `os.inContainer()` over fixture roots: `.dockerenv` → true; `.containerenv` → true;
    `/proc/1/cgroup` with `kubepods` line → true; none → false. Script-level:
    `print(type(os.inContainer()))` → `bool`, and allowed under `--sandbox` (cap-audit row).
- [ ] **Step 2:** implement `cgroup_cpu_quota_at(root) -> Option<usize>` (v2 then v1, spec
  §8.1 — Linux-only cfg; `None` elsewhere), `effective_parallelism()` =
  `$ASCRIPT_WORKERS || min(num_cpus, quota).max(1)`; swap `Pool::new` (`pool.rs:59`) and the
  server's `workers: 0` resolution (`http_server.rs:326`) onto it (one source of truth —
  grep for any other `num_cpus::get` site and classify each in the commit message).
  `os.inContainer` per spec §8.2 with an injectable-root inner fn; ungated (`os` per-func
  `None`; completeness-test row added).
- [ ] **Step 3:** suite + clippy both configs; docs note queued for Phase 7. Commit —
  `feat(runtime): cgroup-aware parallelism (cpu.max v2 + cfs_quota v1) + os.inContainer
  (CNTR §8)`.

### Task 6.2: Phase 6 review

- [ ] Independent reviewer: fixture battery green; on the dev machine (no cgroup) confirms
  behavior identical to `main` (pool cap == num_cpus — run the worker tests); if a Linux/
  Docker environment is available, runs `docker run --cpus=2` with a script printing the pool
  cap and records the observed `2` in the review note (best-effort, not CI-gating).

---

## Phase 7 — Unit G: distribution, examples, docs, negative space, bench, holistic

### Task 7.1: `ascript init --template server` (real template code)

**Files:** `src/main.rs` (the `Init` subcommand), `templates/server/*`, an `init` integration
test in `tests/cli.rs`.

- [ ] **Step 1 (failing test):** `tests/cli.rs`: run `ascript init --template server` into a
  temp dir → all five files exist; `ascript check templates-scaffolded/main.as` exits 0; rerun
  without `--force` → nonzero + conflict list; `--force` overwrites. End-to-end: scaffold, run
  with `PORT=0`-style ephemeral port… (the template reads `env.get("PORT")` — for the test,
  scaffold then patch the port to an ephemeral one, run the binary, curl `/healthz`, send
  SIGTERM, assert clean exit 0 and the drain log line).
- [ ] **Step 2:** the template `main.as` — REAL code (this is the shipped file, verbatim):

```js
// AScript server template — graceful shutdown, healthcheck, container-ready.
import * as server from "std/http/server"
import * as process from "std/process"
import * as task from "std/task"
import * as env from "std/env"
import * as log from "std/log"
import * as time from "std/time"

let started = time.monotonic()
let srv = server.create()

srv.get("/healthz", (req) => {
    return { status: 200, json: { ok: true, uptimeMs: time.monotonic() - started } }
})

srv.get("/", (req) => {
    return { status: 200, body: "hello from ascript\n" }
})

// Example outbound call with retry + backoff (std/task — shipped today).
// Swap to std/resilience policies (circuit breaker, rate limit) when available.
srv.get("/proxy", async (req) => {
    let [v, err] = await task.retry(async () => {
        let [resp, e] = await fetchUpstream()
        if (e != nil) { return [nil, e] }
        return [resp, nil]
    }, { retries: 3, delay: 100, backoff: 2 })
    if (err != nil) { return { status: 502, json: { error: err.message } } }
    return { status: 200, body: v }
})

async fn fetchUpstream() {
    // Replace with a real upstream; kept self-contained for the template.
    return ["ok\n", nil]
}

// Container lifecycle: SIGTERM/SIGINT → stop accepting, drain in-flight, exit.
process.on("SIGTERM", () => srv.shutdown())
process.on("SIGINT", () => srv.shutdown())

let port = int(env.get("PORT") ?? "8080")!
log.info("listening", { port: port })
let [_, err] = await srv.serve({
    port: port,
    workers: 0,            // all cores (cgroup-aware inside a container)
    onShutdown: () => log.info("drain started"),
    drainTimeout: 8000,    // finish in-flight requests within the SIGTERM window
})
if (err != nil) { log.error("serve failed", { error: err.message }) }
```

  *(Implementer: validate every API against the shipped surface before committing — `srv.get`
  handler return shape, `task.retry` opts names, `??`, `int(...)` conversion — adjust to the
  REAL shipped forms where this sketch drifts, and make `ascript check` + a live run the proof.
  The RESIL comment is the §9.3 marked upgrade point — a comment, not a placeholder API.)*
  Plus `Dockerfile` (multi-stage per §9.1: builder runs `ascript build --native`; runtime =
  `debian:bookworm-slim`, non-root `USER`, `STOPSIGNAL SIGTERM`,
  `HEALTHCHECK CMD ["/app", "--health"]`-free — use an http healthcheck via the orchestrator;
  document in template README), `.dockerignore`, `ascript.toml`, `README.md`.
- [ ] **Step 3:** `docker/Dockerfile` (the repo-level base-image file, same multi-stage) +
  **RT-DEPENDENT marked task recorded:** add to this plan's Deferred list + `goal-perf.md`
  CNTR notes: "scratch/distroless variant + published `ascript-rt` base image — blocked on RT
  shipping (`2026-06-12-native-runtime-stubs-design.md`: stub tier matrix §3 + `--oci` §8);
  owner: campaign." **RESIL-DEPENDENT marked task recorded:**
  "template upgrade to `std/resilience` policies — blocked on RESIL shipping
  (`2026-06-12-resilience-stdlib-design.md`); owner: campaign."
- [ ] **Step 4:** suite + clippy. Commit — `feat(cli): ascript init --template server —
  container-ready scaffold (CNTR §9.3)`.

### Task 7.2: examples (Gate 9) + EXAMPLE_SKIPS + four-mode-via-mock

**Files:** `examples/docker_info.as`, `examples/advanced/docker_supervisor.as`,
`tests/vm_differential.rs`, `tests/docker.rs`.

- [ ] **Step 1:** `examples/docker_info.as` (intro): connect (socket from
  `env.get("DOCKER_SOCK")` fallback default), full Tier-1 handling (`docker: unavailable` on
  err — deterministic line), version/info/containers printout. Fmt-idempotent.
- [ ] **Step 2:** `examples/advanced/docker_supervisor.as` (production-shaped, fully
  error-handled): label-filtered `events` watch; on a `die` event for a supervised label,
  inspect + restart with a `task.retry` budget; stream the restarted container's logs (demux
  both streams) with a `tail` window; `process.on("SIGTERM")` → close streams + exit cleanly;
  structured `std/log` throughout.
- [ ] **Step 3:** add both to `EXAMPLE_SKIPS` (reason `DaemonDependent`, per-file comment
  pointing at `tests/docker.rs`); in `tests/docker.rs`, the four-mode test: run EACH example
  under tree-walker / specialized / generic / `.aso` (build then run) with `DOCKER_SOCK`
  pointed at the mock; assert all four outputs byte-identical (and stable across two runs).
  For the supervisor: the mock plays a scripted event sequence then EOF so the example
  terminates deterministically.
- [ ] **Step 4:** Gate-5 sweep (`ascript check` over `examples/**` — 0 `type-*` both configs).
  Suite + clippy. Commit — `feat(examples): docker_info + advanced/docker_supervisor,
  four-mode-proven against the mock daemon (CNTR §10.1)`.

### Task 7.3: docs + NAV + README

**Files:** `docs/content/deploying.md`, `docs/content/stdlib/docker.md`,
`docs/content/stdlib/{net,system,caps}.md`, `docs/content/cli.md`,
`docs/content/language/workers.md`, `docs/assets/app.js`, `README.md`.

- [ ] **Step 1:** write `deploying.md` (spec §9.2 contents) + `stdlib/docker.md` (full API
  reference incl. the Tier split, the demux item shape, the dual-cap requirement stated in a
  callout, the TTY auto-detect note, rootless via `socketPath`).
- [ ] **Step 2:** NAV: `['deploying', 'Deploying & containers']` (Introduction group, after
  `runtime`) and `['stdlib/docker', 'Docker (Engine API)']` (after `stdlib/net`) — the
  NAV-orphan rule. Update `stdlib/net.md` (UDS + `{socketPath}` + the no-reqwest note +
  request-streaming-over-UDS limitation), `stdlib/system.md` (`process.on/off` with the §6.3
  emulated-restore paragraph verbatim-precise + `os.inContainer`), `stdlib/caps.md` (dual-cap +
  the UDS carve-out `unix:<path>` form), `cli.md` (`init`), `workers.md` (cgroup sizing, the
  main-isolate signal rule, drain in the multi-core section), `README.md` (stdlib table row +
  the container story one-liner).
- [ ] **Step 3:** serve the docs site locally (`python3 -m http.server`), click every new/
  changed page + cmd-K search hits. Commit — `docs(cntr): Deploying chapter, std/docker
  reference, UDS/signals/drain/caps pages + NAV`.

### Task 7.4: negative-space test + CLAUDE.md/roadmap/goal-perf

**Files:** `tests/cntr_negative_space.rs`, `CLAUDE.md`, `superpowers/roadmap.md`,
`goal-perf.md`.

- [ ] **Step 1:** `cntr_negative_space.rs` final form: `ASO_FORMAT_VERSION == 27`; the `Op`
  count/list unchanged (pin however the existing negative-space tests pin it — read
  `tests/srv_negative_space.rs` first and mirror); `Value: !Send` assertion still compiles
  (it lives in value.rs — assert here that no new `Value` variant appeared via a
  `size_of`/`type_name`-table pin); worker serializer rejects all four new `NativeKind`s
  (field-path panic, one test each).
- [ ] **Step 2:** CLAUDE.md — a CNTR subsection in "Larger subsystems" (the house terse style:
  the CapReq conjunction + denial order, the http1-not-reqwest decision, the UDS stage-2
  `unix:<path>` rule, the demux auto-detect, signals main-isolate + §6.3 emulated restore,
  drain mechanics + the always-armed select note, cgroup sizing + the one helper,
  the mock-daemon test seam, EXAMPLE_SKIPS entries). `roadmap.md` milestone entry.
  `goal-perf.md`: flip CNTR to ✅ with the two dependency-marked deferrals (RT images, RESIL
  template) recorded inline.
- [ ] **Step 3:** suite green. Commit — `docs(cntr): negative-space pins + CLAUDE/roadmap/
  goal-perf updates`.

### Task 7.5: Gate-12 bench (same-session A/B) + RSS

**Files:** `bench/CNTR_RESULTS.md`.

- [ ] **Step 1:** re-run the vm_bench suite (incl. the DBG zero-cost gate — CNTR touched the
  call_stdlib gate, which is on the stdlib call path) and the server bench against the Task
  0.1 same-session baseline: assert geomean ≈1.0× (all-granted path: the `CapReq` loop is
  unreached; the armed select: behaviorally idle), spec/tw ≥2× floor holds. Record peak RSS on
  the corpus (Gate 18 discipline). Numbers + method into `bench/CNTR_RESULTS.md`.
- [ ] **Step 2:** if ANY regression: fix (it is a bug, never a tradeoff), re-run, then commit —
  `bench(cntr): Gate-12 A/B — all-granted gate + armed accept select cost ≈0 (results)`.

### Task 7.6: final holistic review + gates checklist

- [ ] Holistic review subagent over the WHOLE branch diff: spec-vs-code 1:1 walk (every § with
  a behavior → point at its test); security pass on the two trust boundaries (http1 parser,
  demux) AND the cap conjunction (try to reach a docker call path that skips the gate — e.g.
  via a handle method, a stream `next`, the `for await` native path); cross-subsystem pass
  (the FFI-holistic lesson: any OTHER module that could reach a docker/UDS socket without the
  gate?).
- [ ] Gates checklist, each with run evidence:
  - [ ] Gate 1: four-mode byte-identity — `vm_differential` both configs; the example
    four-mode-via-mock tests green.
  - [ ] Gate 2: clippy clean `--all-targets` AND `--no-default-features --all-targets`.
  - [ ] Gate 3: `cargo test` AND `cargo test --no-default-features` green.
  - [ ] Gate 4: no borrow across await in any new code (reviewer greps the new awaits);
    new handles GC-opaque.
  - [ ] Gate 5: zero `type-*` on `examples/**`, both configs.
  - [ ] Gate 6: no placeholders/silent deferrals — the RT/RESIL deferrals are owner-noted in
    goal-perf; the in-code deferral errors (UDS-on-Windows, attachStdin, body-stream-over-UDS,
    tcp:// DOCKER_HOST) are loud Tier-2/Tier-1 with pinned messages.
  - [ ] Gate 7/9: examples migrated-not-deleted; intro + advanced ship happy+edge.
  - [ ] Gate 8: fuzzers/CI untouched-but-green (no engine surface changed).
  - [ ] Gate 10: unit tests happy+edge incl. the hostile fixture batteries.
  - [ ] Gate 11: tooling parity N/A-but-confirmed (no syntax: fmt idempotence on new examples,
    REPL `import "std/docker"` works, LSP unaffected — run the conformance suites).
  - [ ] Gate 12: Task 7.5 report — zero-cost proven, ≥2× floor holds.
  - [ ] Gate 13: docs + NAV + README done, served-site sanity done.
  - [ ] Gate 14: production-grade — reviewer's explicit hunt over http1/demux/signal/drain
    edges recorded in the review note.
  - [ ] Gates 15–18 (PERF campaign): no new engine config (N/A — recorded); same-session A/B
    done (16); Gate-12 floor re-run (17); RSS recorded (18).
- [ ] Merge `feat/containers-docker` → `main` with `--no-ff` after every box above is ticked.

---

## Deferred (owner-noted, never silent)

- **RT-DEPENDENT:** scratch/distroless base images + published `ascript-rt` image (spec §9.1) —
  blocked on RT shipping (`2026-06-12-native-runtime-stubs-design.md` — specced: stub tier
  matrix §3, `--oci` §8; unshipped).
- **RESIL-DEPENDENT:** template upgrade from `task.retry` to `std/resilience` policies (spec
  §9.3) — blocked on RESIL shipping (`2026-06-12-resilience-stdlib-design.md` — specced,
  unshipped).
- Windows named pipes for docker; docker keep-alive connection pool; interactive exec stdin;
  request streaming over UDS; `std/k8s` (parked with sketch) — all spec §12, each surfaced as a
  loud error or doc note where reachable.
