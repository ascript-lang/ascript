# Phase 5 — Networking & Host Introspection Design

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal.
- **Roadmap:** Phase 5 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Round out the network layer and give a program a view of its host: **DNS lookup**, **UDP
sockets**, **host facts** (pid/platform/arch/cpu-count/hostname), and **live system metrics**
(cpu/mem/disk/load + network interfaces). Powers health endpoints, ops scripts, and the
existing `tui_dashboard` example.

## Sub-phases & feature placement
- **5a — DNS lookup** (`net.lookup`) — `tokio::net::lookup_host`, free, under the existing `net`
  feature.
- **5b — UDP sockets** (`std/net/udp`) — `tokio::net::UdpSocket` native resource, under `net`.
- **5c — Host facts** (`std/os`) — pid/platform/arch/cpuCount (std-only) + hostname (tiny
  `hostname` crate). Under the `sys` feature (OS introspection, like fs/process/env).
- **5d — System metrics** (`std/os` cont.) — cpu/mem/disk/load/uptime + network interfaces via
  the `sysinfo` crate, behind a NEW `sysinfo` Cargo feature (heavy crate; in `default` so the
  full build has it, excluded by `--no-default-features`).

Conventions: stdlib module + (for async) `impl Interp` dispatch; register BOTH `mod.rs` arms,
correctly feature-gated; Tier-1 `[v,err]` for expected failures (DNS resolve failure, bind
failure); Tier-2 panic for misuse; no `RefCell`/resource borrow across `.await`; clippy clean
BOTH configs; RUN both `cargo test` configs; docs + README + example; cancel-on-drop for the UDP
resource.

---

## 5a — DNS lookup (`net.lookup`)

Add to the main `std/net` module (the http/general net module; confirm its name — it routes
`"net"`).
- `net.lookup(host) -> [array<string>, err]` — Tier-1. Resolves a hostname (optionally
  `host:port`) to a list of IP-address strings via `tokio::net::lookup_host`. Resolve failure →
  `[nil, err]`. Async.
- `net.lookupOne(host) -> [string, err]` — convenience: the first resolved address (or err).
- Async dispatch via the interpreter; `lookup_host` needs a `host:port` form — if the caller
  passes a bare host, append a dummy `:0` for resolution and strip the port from the returned
  IP strings (return just the IP). Document.

### Tests (5a)
`net.lookup("localhost")` → array containing `127.0.0.1` and/or `::1`; `net.lookup` of an
invalid host → `[nil, err]`. (Use localhost to avoid network/CI dependence.)

---

## 5b — UDP sockets (`std/net/udp`)

New module `src/stdlib/net_udp.rs`, under `net`. `ResourceState::UdpSocket(tokio::net::UdpSocket)`.
Mirror `net_tcp.rs` (native resource + async methods).
- `udp.bind(addr) -> [socket, err]` — bind to `"host:port"` (e.g. `"0.0.0.0:0"` for ephemeral).
- `udp.send(sock, data, addr) -> [bytesSent, err]` — send `data` (string or bytes) to `addr`.
  async.
- `udp.recv(sock) -> [{data, from}, err]` — receive a datagram; `data` as bytes, `from` the
  sender `"ip:port"`. async. (Use a reasonable max datagram buffer, e.g. 65507.)
- `udp.localAddr(sock) -> string` — the bound local address.
- `udp.close(sock)` — close.
- Take-out-across-await for send/recv; cancel-on-drop (dropping the handle drops the socket).

### Tests (5b)
bind two sockets on `127.0.0.1:0`; send a datagram from A to B's localAddr; recv on B → the
bytes + from-addr; localAddr returns a `127.0.0.1:<port>`; bind to a bad addr → `[nil, err]`.

---

## 5c — Host facts (`std/os`, `sys` feature)

New module `src/stdlib/os.rs`, gated `#[cfg(feature = "sys")]` (OS introspection family).
- `os.pid() -> number` — `std::process::id()`.
- `os.platform() -> string` — `std::env::consts::OS` (e.g. "macos", "linux", "windows").
- `os.arch() -> string` — `std::env::consts::ARCH` (e.g. "aarch64", "x86_64").
- `os.cpuCount() -> number` — `std::thread::available_parallelism()` (fallback 1 on error).
- `os.hostname() -> string` — via the small `hostname` crate (add `hostname = "0.4"` to the `sys`
  feature deps). On error → `"unknown"` or Tier-1? Prefer returning a string ("" / "unknown" on
  failure) since hostname rarely fails; document.
- `os.tempDir() -> string` — `std::env::temp_dir()` (convenience; pairs with fs).
These are mostly sync (pure `os::call`), no await.

### Tests (5c)
pid > 0; platform is one of the known set / non-empty; arch non-empty; cpuCount >= 1; hostname
non-empty; tempDir non-empty + absolute.

---

## 5d — System metrics + interfaces (`std/os` cont., `sysinfo` feature)

Add `sysinfo = "0.31"` to `[dependencies]` (optional) + a new `sysinfo` feature; add `sysinfo`
to the `default` list. The metric functions in `os.rs` are gated `#[cfg(feature = "sysinfo")]`.
- `os.memory() -> {total, used, free, available}` — bytes (numbers).
- `os.swap() -> {total, used}`.
- `os.cpuUsage() -> number` — aggregate CPU % (0–100). NOTE: sysinfo needs two refreshes spaced
  by a short interval for a meaningful %; the call does `refresh → sleep(~200ms) → refresh →
  read` (async, so it doesn't block the event loop). Document the sampling cost.
- `os.loadAvg() -> {one, five, fifteen}` — load averages (unix; on Windows → zeros or nil,
  documented).
- `os.disks() -> array<{mount, total, free, available}>` — mounted filesystems.
- `os.uptime() -> number` — system uptime seconds.
- `os.networkInterfaces() -> array<{name, addresses: array<string>}>` — interfaces + their IPs
  (via sysinfo's `Networks`, or `local-ip-address`/`if-addrs` if sysinfo's network data is
  insufficient — implementer picks; document which).
- `os.localIp() -> [string, err]` — best-effort primary non-loopback IPv4 (via the interfaces or
  a UDP-connect trick); Tier-1.
- **Excluded:** public/outbound IP (that's an HTTP call to an external service — user code, not
  stdlib).

A `sysinfo::System` is stateful; create it per-call (or hold a cached one on `Interp` if cheap —
per-call is simpler and avoids shared-state; document). `cpuUsage`'s sleep uses the take-out
pattern (no borrow across await; the System is a local).

### Tests (5d)
memory.total > 0 and used <= total; cpuUsage in 0..=100 (allow a generous upper tolerance);
disks() is an array (possibly empty in a container — assert it's an array, each entry has the
fields); uptime > 0; networkInterfaces is a non-empty array with a loopback; localIp returns a
string or a clean err. Gate these tests `#[cfg(feature = "sysinfo")]` so `--no-default-features`
excludes them.

---

## Cross-cutting
- NO new language syntax — all builtins/modules; tree-sitter/parser/formatter unaffected;
  conformance passes.
- `--no-default-features` must build & test clean (5a/5b gated `net`, 5c gated `sys`, 5d gated
  `sysinfo`); any integration test that imports these modules must be feature-gated.
- Integration: `examples/host_info.as` (pid/platform/arch/cpuCount/hostname + memory + a DNS
  lookup of localhost + a UDP echo round-trip); docs pages (`std/os`, `std/net` lookup, `std/net/udp`);
  README stdlib table; full gates (both test configs, clippy both, fmt, conformance, idempotence);
  holistic review; merge `--no-ff`.

## Decisions (made; flagged)
1. DNS lookup in the existing `std/net` module (`net.lookup`/`lookupOne`), Tier-1. **Settled.**
2. UDP as `std/net/udp` native-resource module, mirroring `net_tcp`. **Settled.**
3. Host facts + metrics in a new `std/os` module; static facts gated `sys`, live metrics gated a
   new `sysinfo` feature (in `default`). **Settled.**
4. `sysinfo` crate for metrics; `hostname` crate for hostname; public-IP excluded. **Settled.**
5. `cpuUsage` does a two-sample async measurement (~200ms); documented cost. **Settled.**

## Open implementation choices (decide during impl, document)
- Network interfaces source: sysinfo `Networks` vs `local-ip-address`/`if-addrs` — pick whichever
  gives interface name + IP list cleanly.
- `sysinfo::System` per-call vs cached on Interp.
- hostname/loadAvg failure representation (string fallback vs Tier-1) — keep simple, document.
