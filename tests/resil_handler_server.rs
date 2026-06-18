//! RESIL Phase 5, Task 5.3 — end-to-end `resilience.health({checks})` +
//! `resilience.handler(policies, fn)` over a REAL HTTP server. Spawns the BUILT
//! binary running an AScript program that mounts the §6.3 health handler at
//! `/healthz` (liveness) + `/readyz` (a passing + a failing check) and the §6.4
//! policy-wrapped handler at `/quote` (a drained limiter → 429), serving a fixed
//! number of requests (`maxRequests` → deterministic shutdown, process exits 0). The
//! test process issues raw HTTP/1 GETs on an ephemeral port and asserts the mapped
//! statuses + JSON body + `retry-after` header.
//!
//! Covers the full §6.3/§6.4 surface in-server: sync policy boundaries (limiter
//! `tryAcquire` → 429, sync health checks → 503), the expired-on-entry
//! `deadlineMs:0` → 504 path, AND the async deadline RACE (`/slowasync`: a 50ms
//! deadline over an 800ms `await time.sleep` body → 504, body cancelled). The
//! per-connection task establishes a fresh `TASK_LOCALS` scope (`ambient_root_scope`,
//! http_server.rs) so a per-request `resilience.handler({deadlineMs})` can set+read
//! its own deadline (§6.4); without it `call_deadline`'s `try_with` errs and the
//! handler runs the body returning 200 — the `/slow` + `/slowasync` 504s are the
//! in-server proof. (NB: the slow body uses `time.sleep`, the real sleep fn — an
//! earlier draft mis-used the non-existent `task.sleep`, which reads as nil → "value
//! is not callable"; there is NO native-invocation gap.)
//!
//! `net` + `resilience` gated.

#![cfg(all(feature = "net", feature = "resilience"))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Reserve an ephemeral port (bind + drop) so the AScript server can re-bind it.
fn reserve_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// Spawn the built binary running `src` as a `.as` program with `ASC_SERVE_PORT` set.
fn spawn_server(name: &str, src: &str, port: u16) -> Child {
    let file = std::env::temp_dir().join(format!("ascript_resil_handler_{name}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    Command::new(bin)
        .arg("run")
        .arg(&file)
        .env("ASC_SERVE_PORT", port.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn server binary")
}

/// One raw HTTP/1.1 GET, retrying the connect for up to ~6s. Returns the FULL
/// response (status line + headers + body), or `None` on total failure.
fn http_get_full(port: u16, path: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if Instant::now() > deadline {
            return None;
        }
        let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        if stream.write_all(req.as_bytes()).is_err() {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        let mut buf = String::new();
        if stream.read_to_string(&mut buf).is_err() || buf.is_empty() {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        return Some(buf);
    }
}

/// The server program: mounts the §6.3 health + §6.4 handler routes and serves a
/// fixed number of requests. The limiter has capacity 1 (so the 2nd `/quote` is
/// rate-limited → 429), the `/slow` route has `deadlineMs: 0` (always 504), and
/// `/readyz` runs one passing + one failing check (→ 503 degraded).
fn handler_server_program() -> String {
    r#"
import * as server from "std/http/server"
import * as resilience from "std/resilience"
import * as time from "std/time"
import * as env from "std/env"

let lim = resilience.limiter({capacity: 1, refillPerSec: 0.001})
fn quote(req) { return { status: 200, body: "quote-ok" } }
// A genuinely-slow async handler: sleeps far past the deadline so the §5.2 RACE
// (not just expired-on-entry) fires and cancels the body.
async fn slowQuote(req) { await time.sleep(800); return { status: 200, body: "slow-ok" } }

fn pingOk() { return true }
fn pingBad() { return false }

let app = server.create()
app.route("GET", "/healthz", resilience.health({}))
app.route("GET", "/readyz", resilience.health({ checks: { up: pingOk, down: pingBad } }))
app.route("GET", "/quote", resilience.handler({ limiter: lim }, quote))
// deadlineMs:0 is expired-on-entry (sync) → 504 WITHOUT running `quote`.
app.route("GET", "/slow", resilience.handler({ deadlineMs: 0 }, quote))
// deadlineMs:50 with an 800ms async body → the deadline RACE fires → 504, body
// cancelled. Both this and /slow only work because the per-connection task has a
// TASK_LOCALS scope (ambient_root_scope) so `call_deadline` can set+read the local.
app.route("GET", "/slowasync", resilience.handler({ deadlineMs: 50 }, slowQuote))

async fn main() {
  let [port, perr] = int(env.get("ASC_SERVE_PORT"))
  await app.listen("127.0.0.1", port, { maxRequests: 6 })
}
await main()
"#
    .to_string()
}

#[test]
fn health_and_handler_routes_map_statuses() {
    let port = reserve_port();
    let src = handler_server_program();
    let mut child = spawn_server("routes", &src, port);

    // Fetch + on total failure kill the child, drain its piped stderr, and panic.
    fn fetch(child: &mut Child, port: u16, path: &str) -> String {
        match http_get_full(port, path) {
            Some(r) => r,
            None => {
                let _ = child.kill();
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                panic!("no response for {path} within the boot window\nstderr: {err}");
            }
        }
    }

    // 1. Liveness: /healthz → 200 application/json {"status":"ok"}.
    let r = fetch(&mut child, port, "/healthz");
    assert!(r.starts_with("HTTP/1.1 200"), "healthz expected 200:\n{r}");
    assert!(
        r.to_ascii_lowercase().contains("content-type: application/json"),
        "healthz expected json content-type:\n{r}"
    );
    assert!(r.contains("\"status\":\"ok\""), "healthz body:\n{r}");

    // 2. Readiness with a failing check: /readyz → 503 degraded, both checks reported.
    let r = fetch(&mut child, port, "/readyz");
    assert!(r.starts_with("HTTP/1.1 503"), "readyz expected 503:\n{r}");
    assert!(r.contains("\"status\":\"degraded\""), "readyz body:\n{r}");
    assert!(r.contains("\"up\":{\"ok\":true}"), "readyz up check:\n{r}");
    assert!(r.contains("\"down\":{\"ok\":false"), "readyz down check:\n{r}");

    // 3. First /quote consumes the single token → 200.
    let r = fetch(&mut child, port, "/quote");
    assert!(r.starts_with("HTTP/1.1 200"), "first quote expected 200:\n{r}");
    assert!(r.contains("quote-ok"), "first quote body:\n{r}");

    // 4. Second /quote is rate-limited → 429 with a retry-after header.
    let r = fetch(&mut child, port, "/quote");
    assert!(r.starts_with("HTTP/1.1 429"), "second quote expected 429:\n{r}");
    assert!(
        r.to_ascii_lowercase().contains("retry-after:"),
        "429 expected retry-after header:\n{r}"
    );

    // 5. /slow has deadlineMs:0 → expired-on-entry → 504, `quote` never runs.
    // In-server proof that the per-connection task establishes a TASK_LOCALS scope
    // (ambient_root_scope) — without it `call_deadline`'s `try_with` errs, the deadline
    // is never set, and the handler silently runs `quote` returning 200.
    let r = fetch(&mut child, port, "/slow");
    assert!(
        r.starts_with("HTTP/1.1 504"),
        "slow (deadlineMs:0) expected 504 in-server (TASK_LOCALS scope on the connection task):\n{r}"
    );
    assert!(
        !r.contains("quote-ok"),
        "deadlineMs:0 must refuse before running the handler:\n{r}"
    );

    // 6. /slowasync has deadlineMs:50 + an 800ms async body → the deadline RACE fires
    // → 504, the body is cancelled (no "slow-ok"). This exercises the full async
    // enforcement path in-server (the body awaits `time.sleep`, so the race preempts it).
    let r = fetch(&mut child, port, "/slowasync");
    assert!(
        r.starts_with("HTTP/1.1 504"),
        "slowasync (deadlineMs:50, 800ms body) expected 504 via the deadline race:\n{r}"
    );
    assert!(
        !r.contains("slow-ok"),
        "the deadline race must cancel the slow body:\n{r}"
    );

    // The server stopped after maxRequests:6 → process exits 0.
    let status = child.wait().expect("wait for server exit");
    assert!(
        status.success(),
        "server should exit 0 after maxRequests:6, got {status:?}"
    );
}
