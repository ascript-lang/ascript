//! BATT A3 — HTTPS server integration test.
//!
//! Exercises `server.serve({ tls })` (BATT A2) end-to-end: spawns the built binary
//! running an AScript HTTPS server with the baked test TLS credentials, then drives
//! it with a reqwest client trusting the test CA. Asserts HTTP 200 over TLS.
//!
//! The server program is generated inline (like `tests/server_multicore.rs`), NOT
//! the long-running `examples/advanced/https_server.as` (which binds a fixed port
//! and never terminates — it is excluded from automated testing by the `LongRunningServer`
//! skip and is covered by human/manual verification). This test generates a
//! self-terminating HTTPS server (`maxRequests: 1`) on an OS-assigned port, exactly
//! as `server_multicore.rs`'s `tls_multi` module does for multi-isolate TLS.
//!
//! `tls`-gated (requires the rustls stack + reqwest tls feature).
//! `net`-gated (requires `std/http/server`).

#![cfg(all(feature = "tls", feature = "net"))]

use std::process::{Child, Command};
use std::time::Duration;

const CERT: &str = include_str!("../src/stdlib/testdata/tls_test_cert.pem");
const KEY: &str = include_str!("../src/stdlib/testdata/tls_test_key.pem");

// ── helpers (same pattern as server_multicore.rs) ────────────────────────────

/// Reserve an ephemeral port by bind-and-drop, then return it.
fn reserve_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// Write `src` to a temp file and spawn the ascript binary running it.
fn spawn_server(name: &str, src: &str, port: u16) -> Child {
    let file = std::env::temp_dir().join(format!("ascript_tls_srv_{name}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    Command::new(bin)
        .arg("run")
        .arg(&file)
        .env("ASC_SERVE_PORT", port.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tls server binary")
}

/// Build a reqwest HTTPS client trusting ONLY the baked test CA and resolving
/// `localhost` to `127.0.0.1:port` (so the TLS SNI / hostname verification passes).
fn tls_client(port: u16) -> reqwest::Client {
    let ca = reqwest::Certificate::from_pem(CERT.as_bytes()).expect("parse test CA");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    reqwest::Client::builder()
        .add_root_certificate(ca)
        .resolve("localhost", addr)
        .build()
        .expect("build reqwest tls client")
}

/// Poll `https://localhost:port/health` retrying for up to ~8 s (the server may
/// need a moment to compile the script + bind). Returns `Some(status)` on the
/// first response, `None` on timeout.
async fn poll_https(client: &reqwest::Client, port: u16, path: &str) -> Option<u16> {
    let url = format!("https://localhost:{port}{path}");
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        match client.get(&url).send().await {
            Ok(resp) => return Some(resp.status().as_u16()),
            Err(_) => {
                if std::time::Instant::now() > deadline {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        }
    }
}

/// Wait up to `timeout` for the child to exit; kill it if it hasn't.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            return child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ── The minimal self-terminating TLS server program ────────────────────────────

/// Generate a single-isolate HTTPS server program that:
///   - binds on the port from `$ASC_SERVE_PORT`
///   - serves a single GET /health → 200 "ok"
///   - stops after 1 request (`maxRequests: 1`)
///
/// The TLS credentials are the baked test PEM strings (embedded via `format!`).
/// `{{`/`}}` are Rust `format!` escapes for literal `{`/`}`.
fn tls_server_program() -> String {
    format!(
        r#"
import {{ create }} from "std/http/server"
import * as env from "std/env"

let cert = {cert:?}
let key = {key:?}

let app = create()
app.route("GET", "/health", (req) => "ok")

async fn main() {{
  let [port, perr] = int(env.get("ASC_SERVE_PORT"))
  if (perr != nil) {{ print("port env missing"); return }}
  let [bound, berr] = await app.bind("127.0.0.1", port)
  if (berr != nil) {{ print("bind failed: " + berr.message); return }}
  await app.serve({{ maxRequests: 1, tls: {{ cert: cert, key: key }} }})
}}

await main()
"#,
        cert = CERT,
        key = KEY,
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn https_server_serves_200_over_tls() {
    let port = reserve_port();
    let src = tls_server_program();
    let mut child = spawn_server("basic", &src, port);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let status = rt.block_on(async {
        let client = tls_client(port);
        poll_https(&client, port, "/health").await
    });

    // Let the server exit on its own (maxRequests: 1 was served above).
    let exit_status = wait_with_timeout(&mut child, Duration::from_secs(10));
    let _ = exit_status; // we don't assert clean exit (debug-binary startup variance)

    assert_eq!(
        status,
        Some(200),
        "expected HTTP 200 from HTTPS /health over TLS (got {status:?})"
    );
}

/// The `tls_echo.as` example is a four-mode corpus member (tested by
/// `vm_differential`). This test proves it also runs to completion correctly
/// under the BUILT BINARY (not just the in-process test harness), asserting
/// the status line output matches the golden.
#[test]
fn tls_echo_binary_run_produces_correct_output() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/tls_echo.as");

    let output = Command::new(bin)
        .arg("run")
        .arg(&example)
        .output()
        .expect("run tls_echo.as");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "tls_echo.as must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("HTTP/1.1 200 OK"),
        "tls_echo.as must print 'HTTP/1.1 200 OK'; got: {stdout:?}"
    );
}
