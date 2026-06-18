//! RESIL Phase 5, Task 5.3 — end-to-end `resilience.health({checks})` +
//! `resilience.handler(policies, fn)` over a REAL HTTP server. Spawns the BUILT
//! binary running an AScript program that mounts the §6.3 health handler at
//! `/healthz` (liveness) + `/readyz` (a passing + a failing check) and the §6.4
//! policy-wrapped handler at `/quote` (a drained limiter → 429), serving a fixed
//! number of requests (`maxRequests` → deterministic shutdown, process exits 0). The
//! test process issues raw HTTP/1 GETs on an ephemeral port and asserts the mapped
//! statuses + JSON body + `retry-after` header.
//!
//! All exercised paths are SYNCHRONOUS at the policy boundary (limiter `tryAcquire`,
//! sync health checks) — they SIDESTEP two pre-existing, out-of-scope gaps: (a) a
//! module-qualified call inside an async closure invoked by `deadline`/`bulkhead`/
//! `breaker` from native code raises "value is not callable" on BOTH engines; (b) the
//! HTTP server does not establish a `TASK_LOCALS` scope for handler tasks, so a
//! `deadlineMs`-only handler silently no-ops the deadline race in-server (the 504
//! code→status mapping itself is deterministically unit-tested via `deadline(0,fn)`).
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
import * as env from "std/env"

let lim = resilience.limiter({capacity: 1, refillPerSec: 0.001})
fn quote(req) { return { status: 200, body: "quote-ok" } }

fn pingOk() { return true }
fn pingBad() { return false }

let app = server.create()
app.route("GET", "/healthz", resilience.health({}))
app.route("GET", "/readyz", resilience.health({ checks: { up: pingOk, down: pingBad } }))
app.route("GET", "/quote", resilience.handler({ limiter: lim }, quote))

async fn main() {
  let [port, perr] = int(env.get("ASC_SERVE_PORT"))
  await app.listen("127.0.0.1", port, { maxRequests: 4 })
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

    // The server stopped after maxRequests:4 → process exits 0.
    let status = child.wait().expect("wait for server exit");
    assert!(
        status.success(),
        "server should exit 0 after maxRequests:4, got {status:?}"
    );
}
