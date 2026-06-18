//! RESIL Phase 5, Task 5.2 — end-to-end `resilience.metricsHandler()` over a REAL
//! HTTP server. Spawns the BUILT binary running an AScript program that trips a
//! breaker (driving its `breaker_state` gauge to 1 = open), mounts the handler at
//! `/metrics`, and serves ONE request (`maxRequests: 1` → deterministic shutdown,
//! process exits 0). The test process then issues a raw HTTP/1 GET on an ephemeral
//! port and asserts the Prometheus text exposition body + the `text/plain;
//! version=0.0.4` content-type header.
//!
//! `net` + `resilience` gated: the program needs `std/http/server` (`net`) and
//! `std/resilience` (`resilience`). Under either-absent configs this compiles to
//! nothing (the server tier / resilience module are covered by the default build).

#![cfg(all(feature = "net", feature = "resilience"))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Reserve an ephemeral port (bind + drop) so the AScript server can re-bind it.
/// Tiny TOCTOU window; the client's connect-retry keeps this hermetic + non-flaky.
fn reserve_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// Spawn the built binary running `src` as a `.as` program with `ASC_SERVE_PORT` set.
fn spawn_server(name: &str, src: &str, port: u16) -> Child {
    let file = std::env::temp_dir().join(format!("ascript_resil_metrics_{name}.as"));
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

/// One raw HTTP/1.1 GET, retrying the connect for up to ~6s (the server takes a
/// moment to boot + bind). Returns the FULL response (status line + headers + body)
/// so the test can assert the content-type header, or `None` on total failure.
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

/// The server program: trip a breaker to OPEN (4 calls at 50% failure over a window
/// of 4 with minCalls 4 → opens), mount `resilience.metricsHandler()` at `/metrics`,
/// and serve a single request. `breaker_state{name="t"}` is then 1 (open).
fn metrics_server_program() -> String {
    r#"
import * as server from "std/http/server"
import * as resilience from "std/resilience"
import * as env from "std/env"

let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() { return 1 }
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(ok)
b.call(ok)
b.call(fail)
b.call(fail)   // 50% over 4 >= 0.5 → opens

let app = server.create()
app.route("GET", "/metrics", resilience.metricsHandler())

async fn main() {
  let [port, perr] = int(env.get("ASC_SERVE_PORT"))
  // listen = bind(host, port) + serve(opts); single-isolate `serve` needs a
  // pre-bound listener, so we bind the reserved port here directly.
  await app.listen("127.0.0.1", port, { maxRequests: 1 })
}
await main()
"#
    .to_string()
}

#[test]
fn metrics_handler_serves_prometheus_text() {
    let port = reserve_port();
    let src = metrics_server_program();
    let mut child = spawn_server("basic", &src, port);

    let resp = http_get_full(port, "/metrics");
    let resp = match resp {
        Some(r) => r,
        None => {
            let _ = child.kill();
            let out = child.wait_with_output().expect("wait");
            panic!(
                "no /metrics response within the boot window\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
    };

    // Status 200.
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "expected 200, got response:\n{resp}"
    );
    // The custom Prometheus content-type survived value_to_response (header names
    // are case-insensitive; the server lowercases ours).
    assert!(
        resp.to_ascii_lowercase()
            .contains("content-type: text/plain; version=0.0.4"),
        "expected Prometheus content-type header, got:\n{resp}"
    );
    // The body carries the # TYPE gauge line and the open-state series.
    assert!(
        resp.contains("# TYPE ascript_resilience_breaker_state gauge"),
        "expected breaker_state # TYPE line, got:\n{resp}"
    );
    assert!(
        resp.contains("ascript_resilience_breaker_state{name=\"t\"} 1"),
        "expected open breaker_state series, got:\n{resp}"
    );

    // The server stopped after one request (maxRequests:1) → process exits 0.
    let status = child.wait().expect("wait for server exit");
    assert!(
        status.success(),
        "server should exit 0 after maxRequests:1, got {status:?}"
    );
}
