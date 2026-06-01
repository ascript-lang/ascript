# Phase 5 — Networking & Host Introspection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** DNS lookup (`net.lookup`), UDP (`std/net/udp`), host facts + system metrics (`std/os`). Full spec: `docs/superpowers/specs/2026-06-01-phase5-net-host-design.md`.

**Conventions:** native resources for UDP; Tier-1 `[v,err]` for resolve/bind/io failures, Tier-2 panic for misuse; no `RefCell`/resource borrow across `.await` (take-out pattern, mirror `net_tcp.rs`/`io.rs`); feature-gate correctly (5a/5b `net`, 5c `sys`, 5d new `sysinfo` feature in `default`); register BOTH `mod.rs` arms; clippy clean BOTH configs; RUN both `cargo test` configs (feature-gate tests that import gated modules); docs+README+example.

Sub-phases: 5a DNS → 5b UDP → 5c host facts → 5d metrics → integration.

---

## Sub-phase 5a: DNS lookup (`net.lookup`)

**Files:** the main `std/net` module (find which file routes `"net"` — likely `net_http.rs` or a `net.rs`; check `mod.rs` `"net" =>` dispatch), tests.

- [ ] **Step 1 — failing tests:** `net.lookup("localhost")` → array containing `"127.0.0.1"` or `"::1"`; invalid host (e.g. `"nonexistent.invalid"`) → `[nil, <err>]`; `net.lookupOne("localhost")` → a string IP. (Use localhost — no external network dep.)
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** add `lookup`/`lookupOne` to the `net` module's async dispatch (it already has async ops). Use `tokio::net::lookup_host(format!("{host}:0"))` — if the input lacks a `:port`, append `:0`; collect the resolved `SocketAddr`s, return their IP strings (`.ip().to_string()`) de-duplicated, dropping the port. Tier-1 pair. Register in exports() + dispatch (already `net`-gated).
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy.
- [ ] **Step 5 — commit:** `feat(net): net.lookup / net.lookupOne (DNS resolution)`

---

## Sub-phase 5b: UDP sockets (`std/net/udp`)

**Files:** `src/stdlib/net_udp.rs` (new), `src/stdlib/mod.rs` (register `"std/net/udp"` exports + `"udp"` dispatch, `net`-gated), `src/interp.rs` (`ResourceState::UdpSocket`), tests.

- [ ] **Step 1 — failing tests:** bind two sockets on `"127.0.0.1:0"`; `udp.localAddr` returns `127.0.0.1:<port>`; send a datagram from A to B's localAddr; `udp.recv(B)` → `{data, from}` with the bytes and a `127.0.0.1:<port>` from-addr; bind to a bad addr (`"999.999.999.999:0"`) → `[nil, err]`.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** mirror `net_tcp.rs`. `ResourceState::UdpSocket(tokio::net::UdpSocket)`. `udp.bind(addr)→[sock,err]`, `udp.send(sock,data,addr)→[n,err]` (data string→utf8 bytes or bytes), `udp.recv(sock)→[{data:bytes, from:string}, err]` (buffer 65507; take-out-across-await), `udp.localAddr(sock)→string`, `udp.close(sock)`. Register `net`-gated, both arms.
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(net): std/net/udp sockets (bind/send/recv/localAddr/close)`

---

## Sub-phase 5c: Host facts (`std/os`, sys feature)

**Files:** `src/stdlib/os.rs` (new), `mod.rs` (register `"std/os"` + `"os"`, `#[cfg(feature="sys")]`), `Cargo.toml` (`hostname` dep in `sys` feature), tests.

- [ ] **Step 1 — failing tests:** `os.pid() > 0`; `os.platform()` non-empty; `os.arch()` non-empty; `os.cpuCount() >= 1`; `os.hostname()` non-empty; `os.tempDir()` non-empty. (Gate the test `#[cfg(feature="sys")]`.)
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** `os.rs` pure `call`: `pid` (`std::process::id` as f64), `platform` (`std::env::consts::OS`), `arch` (`std::env::consts::ARCH`), `cpuCount` (`std::thread::available_parallelism().map(NonZero::get).unwrap_or(1)`), `hostname` (`hostname` crate; on err → `"unknown"`), `tempDir` (`std::env::temp_dir()`). Add `hostname = "0.4"` to the `sys` feature deps + `dep:hostname`. Register `sys`-gated both arms.
- [ ] **Step 4 — verify:** both configs + clippy (confirm `--no-default-features` excludes `os` cleanly).
- [ ] **Step 5 — commit:** `feat(os): std/os host facts (pid/platform/arch/cpuCount/hostname/tempDir)`

---

## Sub-phase 5d: System metrics (`std/os` cont., sysinfo feature)

**Files:** `src/stdlib/os.rs` (extend, `#[cfg(feature="sysinfo")]` arms), `Cargo.toml` (`sysinfo` optional dep + new `sysinfo` feature + add to `default`), `mod.rs` (no change beyond the os registration; the sysinfo funcs are arms in os.rs gated internally), tests.

- [ ] **Step 1 — failing tests** (gate `#[cfg(feature="sysinfo")]`): `os.memory().total > 0` and `used <= total`; `os.cpuUsage()` in `0..=100` (generous tolerance); `os.disks()` is an array (entries have mount/total/free/available); `os.uptime() > 0`; `os.networkInterfaces()` non-empty array with a loopback; `os.localIp()` → string or clean err.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** add `sysinfo = { version = "0.31", optional = true }` + `sysinfo = ["dep:sysinfo"]` feature + add `"sysinfo"` to `default`. Implement `memory`/`swap`/`cpuUsage`(async two-sample: refresh→`tokio::time::sleep(~200ms)`→refresh→read, no borrow across await — System is a local)/`loadAvg`/`disks`/`uptime`/`networkInterfaces`/`localIp`. Create `sysinfo::System` per call. For interfaces/localIp use sysinfo `Networks` or add `local-ip-address`/`if-addrs` if cleaner (document). The `os` module dispatch must handle these arms only when the feature is on (gate the arms; without the feature they're unknown funcs → normal "no function" error).
- [ ] **Step 4 — verify:** `cargo test` (sysinfo on via default), `cargo test --no-default-features` (sysinfo off — os metrics excluded, builds clean), both clippy. Green.
- [ ] **Step 5 — commit:** `feat(os): system metrics via sysinfo (memory/cpu/disk/load/interfaces)`

---

## Sub-phase 5 integration

- [ ] `examples/host_info.as`: print pid/platform/arch/cpuCount/hostname; memory total; a `net.lookup("localhost")`; a UDP echo round-trip (bind 2 sockets, send/recv on 127.0.0.1). Bounded, terminates, prints success. Conformance (treesitter+frontend) + fmt idempotence.
- [ ] Docs: `std/os` page (host facts + metrics, note sysinfo feature + cpuUsage sampling cost + windows loadAvg caveat), `net.lookup` in the net page, `std/net/udp` page; README stdlib table.
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: borrow-across-await in udp/cpuUsage; Tier-1 vs panic discipline; feature-gating correctness incl. --no-default-features build/test; cancel-on-drop udp; no regression; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 5b UDP recv take-out-across-await + 5d feature-gating (the `sysinfo` arms must not break `--no-default-features`, and the new `default` member must be correct). The 5d reviewer must RUN `cargo test --no-default-features` (not just clippy) to confirm gated tests/modules are excluded.
- cpuUsage's async sleep must not hold any borrow; System is a stack local.
- No new syntax → conformance unchanged.
