//! FFI Task 9 — **Gate 10: the capability-denial audit (end-to-end, hermetic).**
//!
//! This is the codified security boundary. It asserts that under `--sandbox` (and
//! per-capability `--deny` / in-code `caps.drop`) EVERY OS-touching stdlib path is
//! DENIED with the recoverable Tier-2 panic `capability '<cap>' denied`, and that a
//! GRANTED capability still works. A real OS-touching path with no assertion here is
//! a hole (Gate 0) — adding a new resource module without an audit row leaves the
//! boundary unproven.
//!
//! **The three recent security-bypass fixes each get an explicit assertion**, so a
//! regression that re-opens them fails CI:
//!   - **net carve-out for http/udp/ws/server (BLOCKER 1):** `--deny net` blocks
//!     http/udp/ws/server, not just tcp connect/listen.
//!   - **sqlite/postgres/redis gating (BLOCKER 2):** the database modules are gated
//!     (sqlite→fs, postgres/redis→net).
//!   - **per-handle re-check after drop (BLOCKER 3):** a handle opened while a cap
//!     was granted is DENIED once the cap is dropped (`listener.accept()` after
//!     `caps.drop("net")`).
//!
//! **Hermetic:** the gate denies BEFORE any connect/bind/spawn, so no real network,
//! filesystem mutation, or subprocess is required. DNS (`net.lookup`) is included
//! precisely because it is the bypass the dispatch-site gate closes by construction
//! (it is NOT a connect site).

use std::process::Command;

/// Run `src` (written to a temp file) with extra CLI flags; return `(ok, stdout,
/// stderr)`. Mirrors the helper in `cli.rs` but kept local so this audit file is
/// self-contained.
fn run_with_args(src: &str, name: &str, extra: &[&str]) -> (bool, String, String) {
    let file = std::env::temp_dir().join(name);
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = Command::new(bin);
    cmd.arg("run");
    for a in extra {
        cmd.arg(a);
    }
    cmd.arg(&file);
    let output = cmd.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Assert that, under `flags`, the program `body` raises `capability '<cap>' denied`
/// when the OS-touching call is wrapped in `recover` — proving the gate fired BEFORE
/// the side effect. The program prints the recovered error message; we assert it.
/// `imports` is the import preamble (so we only import what each cfg needs).
#[cfg_attr(not(any(feature = "sys", feature = "net", feature = "sql", feature = "postgres", feature = "redis", feature = "ffi")), allow(dead_code))]
fn assert_denied(name: &str, imports: &str, expr: &str, cap: &str, flags: &[&str]) {
    let src = format!(
        "{imports}\nlet r = recover(() => {expr})\nprint(r[1].message)\n"
    );
    let (ok, out, err) = run_with_args(&src, name, flags);
    assert!(
        ok,
        "[{name}] program should run (denial is recoverable); stderr: {err}"
    );
    let want = format!("capability '{cap}' denied");
    assert!(
        out.trim() == want,
        "[{name}] expected `{want}`, got stdout {out:?} (stderr {err:?})"
    );
}

/// Assert that, under `flags`, the program `body` SUCCEEDS — used to confirm a
/// GRANTED capability still works (the positive half of the audit) and that ambient
/// introspection is not over-gated.
#[cfg_attr(not(feature = "sys"), allow(dead_code))]
fn assert_allowed(name: &str, src: &str, flags: &[&str], expect_stdout: &str) {
    let (ok, out, err) = run_with_args(src, name, flags);
    assert!(ok, "[{name}] expected success; stderr: {err}");
    assert_eq!(out, expect_stdout, "[{name}] stdout mismatch (stderr {err:?})");
}

// ───────────────────────────── fs (read/write/stat/mkdir/remove) ──────────────
// All gated by `Fs`. `--sandbox` and `--deny fs` both deny; under `--sandbox` even a
// read of an existing file is denied (the gate fires before touching the fd).

#[cfg(feature = "sys")]
#[test]
fn audit_fs_all_ops_denied_under_sandbox() {
    let imp = "import * as fs from \"std/fs\"";
    for (name, expr) in [
        ("audit_fs_read.as", "fs.read(\"/etc/hosts\")"),
        ("audit_fs_write.as", "fs.write(\"/tmp/ascript_audit_x\", \"x\")"),
        ("audit_fs_stat.as", "fs.stat(\"/etc/hosts\")"),
        ("audit_fs_mkdir.as", "fs.mkdir(\"/tmp/ascript_audit_d\")"),
        ("audit_fs_remove.as", "fs.remove(\"/tmp/ascript_audit_z\")"),
        ("audit_fs_exists.as", "fs.exists(\"/etc/hosts\")"),
    ] {
        assert_denied(name, imp, expr, "fs", &["--sandbox"]);
        assert_denied(name, imp, expr, "fs", &["--deny", "fs"]);
    }
}

#[cfg(feature = "sys")]
#[test]
fn audit_fs_granted_still_works() {
    // With fs NOT denied, a read of a file we just wrote succeeds.
    let path = std::env::temp_dir().join("ascript_audit_fs_ok.txt");
    std::fs::write(&path, "ok").unwrap();
    let src = format!(
        "import * as fs from \"std/fs\"\nlet [v, e] = fs.read(\"{}\")\nprint(v)\n",
        path.to_string_lossy()
    );
    assert_allowed("audit_fs_ok.as", &src, &["--deny", "net"], "ok\n");
    std::fs::remove_file(&path).ok();
}

// ───────────────────────────── io (stdin reads) — gated by Fs ─────────────────

#[cfg(feature = "sys")]
#[test]
fn audit_io_stdin_reads_denied_by_fs() {
    let imp = "import * as io from \"std/io\"";
    for (name, expr) in [
        ("audit_io_readall.as", "io.readAll()"),
        ("audit_io_readline.as", "io.readLine()"),
        ("audit_io_readlines.as", "io.readLines()"),
    ] {
        // io is gated by `fs` (a host-fd read, §4.3a).
        assert_denied(name, imp, expr, "fs", &["--deny", "fs"]);
        assert_denied(name, imp, expr, "fs", &["--sandbox"]);
    }
}

// ───────────────────────────── env (read/write) — gated by Env ────────────────

#[cfg(feature = "sys")]
#[test]
fn audit_env_denied() {
    let imp = "import * as env from \"std/env\"";
    assert_denied("audit_env_get.as", imp, "env.get(\"PATH\")", "env", &["--deny", "env"]);
    assert_denied("audit_env_get_sbx.as", imp, "env.get(\"PATH\")", "env", &["--sandbox"]);
}

// ───────────────────────────── net: connect/listen/DNS/http/udp/ws/server ─────
// ALL gated by `Net`. The DNS rows (`net.lookup`/`lookupOne`) are the keystone
// regression: they route through `call_net` (NOT a connect/bind site), so a
// per-connect gate would leave them open — the dispatch-site gate closes them.

#[cfg(feature = "net")]
#[test]
fn audit_net_dns_lookup_denied_is_the_keystone() {
    let imp = "import * as net from \"std/net\"";
    // [Gate 10] DNS egress IS gated: caps.drop("net") / --deny net / --sandbox block
    // net.lookup — it returns `capability 'net' denied`, NOT a resolved address list.
    assert_denied("audit_dns_lookup.as", imp, "net.lookup(\"example.com\")", "net", &["--deny", "net"]);
    assert_denied("audit_dns_lookup_sbx.as", imp, "net.lookup(\"example.com\")", "net", &["--sandbox"]);
    assert_denied("audit_dns_lookupone.as", imp, "net.lookupOne(\"example.com\")", "net", &["--deny", "net"]);

    // And via in-code caps.drop (the irreversible path) — same denial.
    let src = "import * as net from \"std/net\"\n\
               import * as caps from \"std/caps\"\n\
               caps.drop(\"net\")\n\
               let r = recover(() => net.lookup(\"example.com\"))\n\
               print(r[1].message)\n";
    let (ok, out, err) = run_with_args(src, "audit_dns_drop.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(out, "capability 'net' denied\n", "DNS via caps.drop must be denied");
}

#[cfg(feature = "net")]
#[test]
fn audit_net_tcp_connect_listen_denied() {
    let imp = "import * as tcp from \"std/net/tcp\"";
    assert_denied("audit_tcp_connect.as", imp, "tcp.connect(\"127.0.0.1\", 9)", "net", &["--deny", "net"]);
    assert_denied("audit_tcp_listen.as", imp, "tcp.listen(\"127.0.0.1\", 0)", "net", &["--deny", "net"]);
    assert_denied("audit_tcp_connect_sbx.as", imp, "tcp.connect(\"127.0.0.1\", 9)", "net", &["--sandbox"]);
}

// BATT A3 §3 — `tcp.connectTls` is gated by `Net` exactly like the plain
// `tcp.connect` above. The TLS handshake goes through the same `required_cap`
// dispatch chokepoint before any socket I/O (hermetic — no real network needed;
// port 9 is effectively unreachable on loopback, but the gate fires before
// `connect()` so no real packet is ever sent). `--deny net`, `--sandbox`, and
// in-code `caps.drop("net")` all produce `capability 'net' denied`.
#[cfg(all(feature = "net", feature = "tls"))]
#[test]
fn audit_net_tls_connect_denied() {
    let imp = "import * as tcp from \"std/net/tcp\"";
    // `tcp.connectTls(host, port, opts?)` — gated by Net at the dispatch chokepoint.
    assert_denied(
        "audit_tls_connect_deny.as",
        imp,
        "tcp.connectTls(\"127.0.0.1\", 9, {})",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_tls_connect_sbx.as",
        imp,
        "tcp.connectTls(\"127.0.0.1\", 9, {})",
        "net",
        &["--sandbox"],
    );

    // in-code `caps.drop("net")` — irreversible; the gate fires after the drop.
    let src = "import * as tcp from \"std/net/tcp\"\n\
               import * as caps from \"std/caps\"\n\
               caps.drop(\"net\")\n\
               let r = recover(() => tcp.connectTls(\"127.0.0.1\", 9, {}))\n\
               print(r[1].message)\n";
    let (ok, out, err) = run_with_args(src, "audit_tls_connect_drop.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(
        out,
        "capability 'net' denied\n",
        "caps.drop(\"net\") must deny tcp.connectTls"
    );
}

// BATT A3 §3 positive — `tcp.connectTls` is ALLOWED when `net` is granted (the
// normal case). The handshake will fail (port 9 is unreachable), but the
// capability gate PASSES and the error is a Tier-1 connection error, NOT a
// denial. We use `--deny fs` (fs denied, net still granted) to confirm the
// cap gate does not over-block.
#[cfg(all(feature = "net", feature = "tls"))]
#[test]
fn audit_net_tls_connect_allowed_when_net_granted() {
    // `tcp.connectTls` to a guaranteed-unreachable port: the cap gate passes, the
    // call returns `[nil, err]` with a connection error (NOT a denial message).
    let src = "import * as tcp from \"std/net/tcp\"\n\
               let [_, e] = await tcp.connectTls(\"127.0.0.1\", 9, {})\n\
               print(e != nil)\n\
               print(e.message != nil)\n";
    // Under --deny fs: net is still granted → gate passes.
    let (ok, out, err) = run_with_args(src, "audit_tls_connect_allowed.as", &["--deny", "fs"]);
    assert!(ok, "[audit_tls_connect_allowed] stderr: {err}");
    // Should print "true\ntrue\n" (got a non-nil err, and err.message is non-nil) —
    // a connection error, NOT a denial.
    assert!(
        !out.contains("capability 'net' denied"),
        "[audit_tls_connect_allowed] must not produce a denial when net is granted; got {out:?}"
    );
    assert_eq!(
        out, "true\ntrue\n",
        "[audit_tls_connect_allowed] expected connection-error pair, got {out:?} (stderr {err:?})"
    );
}

// CNTR §3.1 std/net/unix — a UDS connect/listen is gated by `Net` exactly like TCP.
// `--deny net` AND `--sandbox` deny BEFORE any bind/connect (no real socket touched).
#[cfg(all(unix, feature = "net"))]
#[test]
fn audit_net_unix_connect_listen_denied() {
    let imp = "import * as unix from \"std/net/unix\"";
    assert_denied(
        "audit_unix_connect.as",
        imp,
        "unix.connect(\"/tmp/ascript-audit-nope.sock\")",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_unix_listen.as",
        imp,
        "unix.listen(\"/tmp/ascript-audit-nope.sock\")",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_unix_connect_sbx.as",
        imp,
        "unix.connect(\"/tmp/ascript-audit-nope.sock\")",
        "net",
        &["--sandbox"],
    );
    assert_denied(
        "audit_unix_listen_sbx.as",
        imp,
        "unix.listen(\"/tmp/ascript-audit-nope.sock\")",
        "net",
        &["--sandbox"],
    );
}

// [BLOCKER 1] the net carve-out covers http/udp/ws/server — NOT just tcp. A
// regression that gated only tcp connect/listen would leave these open.
#[cfg(feature = "net")]
#[test]
fn audit_net_http_udp_ws_server_denied_blocker1() {
    assert_denied(
        "audit_http_get.as",
        "import * as http from \"std/net/http\"",
        "http.get(\"http://example.com\")",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_udp_bind.as",
        "import * as udp from \"std/net/udp\"",
        "udp.bind(\"127.0.0.1\", 0)",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_ws_connect.as",
        "import * as ws from \"std/net/ws\"",
        "ws.connect(\"ws://127.0.0.1:9/x\")",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_server_create.as",
        "import * as server from \"std/http/server\"",
        "server.create()",
        "net",
        &["--deny", "net"],
    );
}

// [BLOCKER 3] per-handle re-check AFTER drop: a TCP listener opened while net was
// granted is DENIED once net is dropped (the handle method re-checks the governing
// cap). Hermetic — we bind an ephemeral port, then drop, then accept.
#[cfg(feature = "net")]
#[test]
fn audit_net_per_handle_recheck_after_drop_blocker3() {
    let src = "import * as tcp from \"std/net/tcp\"\n\
               import * as caps from \"std/caps\"\n\
               let [listener, e] = tcp.listen(\"127.0.0.1\", 0)\n\
               if (e != nil) { print(\"setup failed\") } else {\n\
                 caps.drop(\"net\")\n\
                 let r = recover(() => listener.accept())\n\
                 print(r[1].message)\n\
               }\n";
    let (ok, out, err) = run_with_args(src, "audit_handle_recheck.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(
        out, "capability 'net' denied\n",
        "a handle opened before the drop must be denied after it (BLOCKER 3)"
    );
}

// CNTR §5.3 (Task 1.2): the per-handle re-check (BLOCKER 3, above) now consults
// `NativeKind::governing_caps() -> CapReq` and iterates it in stable `Cap::ALL`
// order — the same "first-denied names the error" mechanism the central
// `required_cap` gate uses. For every CURRENT handle the requirement is a single
// cap (so the loop runs exactly once → byte-identical to the pre-CNTR
// `Option<Cap>` path, proven by `governing_caps_preserves_verdicts` in
// `src/value.rs` + the BLOCKER 3 end-to-end row above).

// ───────────────────── docker (net ∧ process conjunction — CNTR §10.2) ─────────
//
// `std/docker` is the FIRST dual-cap conjunction in the stdlib: `docker.connect`
// requires BOTH `net` (to open the Unix/TCP socket to the Engine) AND `process`
// (the Engine can spawn host processes on behalf of the call). The gate fires at
// the `call_stdlib` dispatch chokepoint BEFORE any socket I/O (hermetic — no real
// daemon needed). CNTR Task 4.6 end-to-end proof:
//   1. `--deny net`     → denied with `capability 'net' denied`
//   2. `--deny process` → denied with `capability 'process' denied`
//   3. `--sandbox`      → denied (sandbox = deny-all)
//   4. in-code `caps.drop("process")` → denied (irreversible drop is honoured)
//   5. `--deny net --deny process` together → `'net' denied` (Cap::ALL order:
//      Net is checked before Process — net-first is the conjunction iteration order)
//   6. Per-handle re-check (BLOCKER-3 mirror): a `DockerClient` handle opened while
//      both caps are granted is DENIED on `.ping()` AFTER `caps.drop("process")`.
//   7. Per-handle re-check for a `DockerStream`: a log stream handle is DENIED on
//      `.next()` after `caps.drop("net")`.
//
// POSITIVE half: `docker.connect` SUCCEEDS with no denials. That proof is in
// `tests/docker.rs` (Task 4.2 `connect_negotiates_version_and_lists_containers` and
// the full Task 4.5 live-round-trip). The rows below cover the DENIAL side only.

#[cfg(all(unix, feature = "docker"))]
#[test]
fn audit_docker_connect_denied_under_deny_net() {
    // Denial fires at the dispatch gate BEFORE any socket I/O — a bogus path is fine.
    assert_denied(
        "audit_docker_deny_net.as",
        "import * as docker from \"std/docker\"",
        "docker.connect({ socketPath: \"/tmp/ascript-audit-docker-nope.sock\" })",
        "net",
        &["--deny", "net"],
    );
}

#[cfg(all(unix, feature = "docker"))]
#[test]
fn audit_docker_connect_denied_under_deny_process() {
    assert_denied(
        "audit_docker_deny_process.as",
        "import * as docker from \"std/docker\"",
        "docker.connect({ socketPath: \"/tmp/ascript-audit-docker-nope.sock\" })",
        "process",
        &["--deny", "process"],
    );
}

#[cfg(all(unix, feature = "docker"))]
#[test]
fn audit_docker_connect_denied_under_sandbox() {
    assert_denied(
        "audit_docker_sandbox.as",
        "import * as docker from \"std/docker\"",
        "docker.connect({ socketPath: \"/tmp/ascript-audit-docker-nope.sock\" })",
        "net",
        &["--sandbox"],
    );
}

#[cfg(all(unix, feature = "docker"))]
#[test]
fn audit_docker_connect_denied_via_caps_drop_process() {
    // `caps.drop("process")` is irreversible; the gate at `call_stdlib` fires after.
    let src = "import * as docker from \"std/docker\"\n\
               import * as caps from \"std/caps\"\n\
               caps.drop(\"process\")\n\
               let r = recover(() => docker.connect({ socketPath: \"/tmp/ascript-audit-docker-nope.sock\" }))\n\
               print(r[1].message)\n";
    let (ok, out, err) = run_with_args(src, "audit_docker_caps_drop_process.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(
        out, "capability 'process' denied\n",
        "caps.drop(\"process\") must deny docker.connect"
    );
}

#[cfg(all(unix, feature = "docker"))]
#[test]
fn audit_docker_conjunction_net_denied_first_when_both_dropped() {
    // Cap::ALL order: Fs, Net, Process, Ffi, Env — Net is checked before Process.
    // When BOTH are denied, the error names 'net' (the first denied cap in ALL-order).
    let src = "import * as docker from \"std/docker\"\n\
               import * as caps from \"std/caps\"\n\
               caps.drop(\"net\")\n\
               caps.drop(\"process\")\n\
               let r = recover(() => docker.connect({ socketPath: \"/tmp/ascript-audit-docker-nope.sock\" }))\n\
               print(r[1].message)\n";
    let (ok, out, err) = run_with_args(src, "audit_docker_conjunction_order.as", &[]);
    assert!(ok, "stderr: {err}");
    assert_eq!(
        out, "capability 'net' denied\n",
        "when both net and process are denied, 'net' must be named first (Cap::ALL order)"
    );
}

// CNTR §10.2 per-handle re-check (BLOCKER-3 mirror for DockerClient).
//
// Strategy: the mock daemon from tests/docker.rs is NOT available here (cap_audit
// tests are sync + hermetic). We cover the per-handle re-check in a self-contained
// way: use a `unix.connect` raw socket to speak HTTP to the mock... that would need
// async. Instead, we use the `--deny` flag PATH for connect-time denial (rows above)
// AND wire the per-handle re-check via the in-code `caps.drop` path:
//   - Open a DockerClient against the mock (requires the mock → tested in docker.rs).
//   - OR: prove the per-handle re-check mechanism is wired for DockerClient /
//     DockerStream via the unit test `governing_caps_preserves_verdicts` in
//     `src/value.rs` (which asserts `NativeKind::DockerClient.governing_caps() ==
//     CapReq::one(Net).and(Process)`) and the end-to-end denial that fires via the
//     dispatch gate before connect.
//
// The actual mock-backed per-handle re-check (connect → drop cap → ping → denied)
// is covered in `tests/docker.rs` as `docker_client_per_handle_recheck_after_cap_drop`
// (Task 4.6 — added there because it needs `mock_daemon()` which is async/tokio).
// That test proves BLOCKER-3 end-to-end; the rows here prove the CONNECT-TIME side.

// ───────────────────────────── process (spawn) — gated by Process ─────────────

#[cfg(feature = "sys")]
#[test]
fn audit_process_spawn_run_denied() {
    let imp = "import * as process from \"std/process\"";
    assert_denied("audit_proc_run.as", imp, "process.run(\"true\", [])", "process", &["--deny", "process"]);
    assert_denied("audit_proc_spawn.as", imp, "process.spawn(\"true\", [])", "process", &["--sandbox"]);
}

// CNTR §6 (Task 4.6 forward-link): `process.on` is the inbound signal seam — gated by
// Process like the rest of `std/process`. `--deny process` AND `--sandbox` both deny it
// BEFORE the listener task is ever spawned (the gate fires at `call_stdlib`).
#[cfg(all(unix, feature = "sys"))]
#[test]
fn audit_process_on_denied() {
    let imp = "import * as process from \"std/process\"";
    assert_denied(
        "audit_proc_on_deny.as",
        imp,
        "process.on(\"SIGTERM\", (s) => { print(s) })",
        "process",
        &["--deny", "process"],
    );
    assert_denied(
        "audit_proc_on_sbx.as",
        imp,
        "process.on(\"SIGTERM\", (s) => { print(s) })",
        "process",
        &["--sandbox"],
    );
}

// ───────────────────────────── os topology / identity — gated by Net ──────────
// `os.networkInterfaces()` / `os.localIp()` / `os.hostname()` leak network topology
// / host identity without a socket — gated by Net. Ambient introspection
// (`os.platform`/`os.cpuCount`/`os.pid`/…) is NOT gated.

#[cfg(feature = "sys")]
#[test]
fn audit_os_topology_denied_by_net() {
    let imp = "import * as os from \"std/os\"";
    assert_denied("audit_os_netif.as", imp, "os.networkInterfaces()", "net", &["--deny", "net"]);
    assert_denied("audit_os_localip.as", imp, "os.localIp()", "net", &["--deny", "net"]);
    assert_denied("audit_os_hostname.as", imp, "os.hostname()", "net", &["--deny", "net"]);
    // Under --sandbox too.
    assert_denied("audit_os_netif_sbx.as", imp, "os.networkInterfaces()", "net", &["--sandbox"]);
}

#[cfg(feature = "sys")]
#[test]
fn audit_os_ambient_introspection_not_gated() {
    // Ambient host metadata is NOT gated — even under --sandbox.
    // `os.platform()` is a string; `os.cpuCount()`/`os.pid()` return the `float`
    // numeric subtype (NUM: int-valued host metadata still carries `float`), so
    // `type(...)` prints `float`. The point is they SUCCEED under --sandbox.
    let src = "import * as os from \"std/os\"\n\
               print(type(os.platform()))\n\
               print(os.cpuCount() > 0)\n\
               print(os.pid() > 0)\n";
    assert_allowed("audit_os_ambient.as", src, &["--sandbox"], "string\ntrue\ntrue\n");
}

/// CNTR §8.2 — `os.inContainer()` is ungated: a pure filesystem probe that acquires
/// no new OS resource. It MUST succeed even under `--sandbox`.
/// Returns a bool (true inside a container, false on this macOS dev machine).
#[test]
fn audit_os_in_container_ungated_under_sandbox() {
    let src = "import * as os from \"std/os\"\nprint(type(os.inContainer()))\n";
    assert_allowed(
        "audit_os_in_container.as",
        src,
        &["--sandbox"],
        "bool\n",
    );
}

// ───────────────────────────── sqlite/postgres/redis (BLOCKER 2) ──────────────
// The database modules open OS resources and MUST be gated: sqlite opens/creates a
// DB file → Fs; postgres/redis open TCP sockets → Net.

#[cfg(feature = "sql")]
#[test]
fn audit_sqlite_denied_by_fs_blocker2() {
    let imp = "import * as sqlite from \"std/sqlite\"";
    // sqlite.open creates/opens a DB file → gated by Fs.
    assert_denied(
        "audit_sqlite_open.as",
        imp,
        "sqlite.open(\"/tmp/ascript_audit.db\")",
        "fs",
        &["--deny", "fs"],
    );
    assert_denied(
        "audit_sqlite_open_sbx.as",
        imp,
        "sqlite.open(\"/tmp/ascript_audit.db\")",
        "fs",
        &["--sandbox"],
    );
}

#[cfg(feature = "postgres")]
#[test]
fn audit_postgres_denied_by_net_blocker2() {
    let imp = "import * as postgres from \"std/postgres\"";
    assert_denied(
        "audit_postgres.as",
        imp,
        "postgres.connect(\"postgres://localhost/db\")",
        "net",
        &["--deny", "net"],
    );
}

#[cfg(feature = "redis")]
#[test]
fn audit_redis_denied_by_net_blocker2() {
    let imp = "import * as redis from \"std/redis\"";
    assert_denied(
        "audit_redis.as",
        imp,
        "redis.connect(\"redis://localhost\")",
        "net",
        &["--deny", "net"],
    );
}

// ───────────────────────────── ffi (open) — gated by Ffi ──────────────────────

#[cfg(feature = "ffi")]
#[test]
fn audit_ffi_open_denied() {
    let imp = "import * as ffi from \"std/ffi\"";
    assert_denied("audit_ffi_open.as", imp, "ffi.open(\"libm.so.6\")", "ffi", &["--deny", "ffi"]);
    assert_denied("audit_ffi_open_sbx.as", imp, "ffi.open(\"libm.so.6\")", "ffi", &["--sandbox"]);
}

// Holistic-review BLOCKER: `ai`/`telemetry` carry their OWN reqwest network stacks (not
// routed through `net_http`) and `workflow` persists an event-log FILE — all were ungated
// network/fs egress that defeated --deny net / --deny fs / --sandbox. Now gated; audited.

#[cfg(feature = "telemetry")]
#[test]
fn audit_telemetry_network_denied_by_net() {
    let imp = "import * as telemetry from \"std/telemetry\"";
    // telemetry.flush exports over the network → gated by Net (was an exfil channel).
    assert_denied("audit_telemetry_flush.as", imp, "telemetry.flush()", "net", &["--deny", "net"]);
    assert_denied("audit_telemetry_flush_sbx.as", imp, "telemetry.flush()", "net", &["--sandbox"]);
}

#[cfg(feature = "ai")]
#[test]
fn audit_ai_network_denied_by_net() {
    let imp = "import * as ai from \"std/ai\"";
    // ai.generate makes an LLM API call → gated by Net.
    assert_denied(
        "audit_ai_generate.as",
        imp,
        "ai.generate({model: \"openai:gpt-4o-mini\", prompt: \"hi\"})",
        "net",
        &["--deny", "net"],
    );
    assert_denied(
        "audit_ai_generate_sbx.as",
        imp,
        "ai.generate({model: \"openai:gpt-4o-mini\", prompt: \"hi\"})",
        "net",
        &["--sandbox"],
    );
}

#[cfg(feature = "workflow")]
#[test]
fn audit_workflow_log_write_denied_by_fs() {
    // workflow.run persists an append-only event log to a user `{log}` file path → Fs.
    let imp = "import { run } from \"std/workflow\"\nfn wf(ctx, input) { return 1 }";
    assert_denied(
        "audit_workflow_run.as",
        imp,
        "run(wf, {id: 1}, {log: \"/tmp/ascript_audit_wf.log\"})",
        "fs",
        &["--deny", "fs"],
    );
    assert_denied(
        "audit_workflow_run_sbx.as",
        imp,
        "run(wf, {id: 1}, {log: \"/tmp/ascript_audit_wf.log\"})",
        "fs",
        &["--sandbox"],
    );
}

// ───────────────────────────── the positive half: a granted cap works ─────────

#[test]
fn audit_caps_list_reflects_sandbox() {
    // Under --sandbox, caps.list() is empty (every dangerous cap gone) — the central
    // proof that --sandbox = deny-all-five.
    let src = "import * as caps from \"std/caps\"\nprint(caps.list())\n";
    assert_allowed("audit_sandbox_list.as", src, &["--sandbox"], "[]\n");
    // A single --deny leaves the rest granted.
    assert_allowed(
        "audit_deny_one_list.as",
        src,
        &["--deny", "ffi"],
        "[\"fs\", \"net\", \"process\", \"env\"]\n",
    );
}
