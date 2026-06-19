//! BATT B6 §8.2 — `std/email` SMTP CLIENT tests against an in-process SCRIPTED
//! SMTP stub.
//!
//! The stub is a tiny tokio server (its OWN current-thread runtime on a dedicated
//! std::thread) that speaks a canned SMTP dialogue and RECORDS the commands the
//! client sent. The `.as` script connects to `127.0.0.1:<port>` and drives the
//! real `email.send`/`email.connect` state machine; the test then asserts BOTH the
//! script's stdout (the Tier-1 result / panic) AND the wire transcript the stub
//! captured (dot-stuffing, AUTH base64, the re-EHLO after STARTTLS, etc).
//!
//! SECURITY pins:
//!   - STARTTLS-strip: `tls:"starttls"` requested but the server does NOT advertise
//!     STARTTLS (or the upgrade fails) → Tier-1, the client REFUSES to send MAIL/AUTH
//!     in plaintext (never a silent downgrade).
//!   - AUTH without TLS → Tier-2 (a programmer error) unless `allowInsecureAuth:true`.
//!   - WIRE-LAYER CRLF: a hand-built msg object with a `\r\n` smuggled into a `to`
//!     address is re-rejected at send time (Tier-2), even though it bypassed the
//!     builder's `reject_crlf`.

#![cfg(all(feature = "email", feature = "net", feature = "tls"))]

use std::io::Write as _;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CERT: &str = include_str!("../src/stdlib/testdata/tls_test_cert.pem");
const KEY: &str = include_str!("../src/stdlib/testdata/tls_test_key.pem");

/// One scripted step: read a client command line (and assert it matches `expect`
/// if `Some`), then write `reply`. A `None` expect means "read whatever the client
/// sends but don't assert it". `upgrade_tls` (after writing `reply`) wraps the
/// server side in a rustls acceptor (for STARTTLS). `stall` sleeps before replying
/// (for the timeout test).
struct Step {
    expect: Option<String>,
    /// For DATA: read until a lone `.` line (the message terminator) instead of one line.
    read_data: bool,
    reply: String,
    upgrade_tls: bool,
    stall: Option<Duration>,
}

impl Step {
    fn line(expect: Option<&str>, reply: &str) -> Step {
        Step { expect: expect.map(String::from), read_data: false, reply: reply.into(), upgrade_tls: false, stall: None }
    }
    fn data(reply: &str) -> Step {
        Step { expect: None, read_data: true, reply: reply.into(), upgrade_tls: false, stall: None }
    }
    fn upgrade(expect: &str, reply: &str) -> Step {
        Step { expect: Some(expect.into()), read_data: false, reply: reply.into(), upgrade_tls: true, stall: None }
    }
    fn stall(secs: u64, reply: &str) -> Step {
        Step { expect: None, read_data: false, reply: reply.into(), upgrade_tls: false, stall: Some(Duration::from_secs(secs)) }
    }
}

/// Build the rustls server config from the shared test cert (server side of STARTTLS).
fn server_tls_config() -> Arc<tokio_rustls::rustls::ServerConfig> {
    use tokio_rustls::rustls;
    let certs: Vec<_> = rustls_pemfile::certs(&mut CERT.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut KEY.as_bytes()).unwrap().unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    Arc::new(cfg)
}

/// Either a plaintext or a TLS-upgraded server-side stream. We read/write line-wise.
enum Sock {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl Sock {
    async fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        match self {
            Sock::Plain(s) => s.write_all(b).await,
            Sock::Tls(s) => s.write_all(b).await,
        }
    }
    async fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Sock::Plain(s) => s.read(buf).await,
            Sock::Tls(s) => s.read(buf).await,
        }
    }
}

/// A line-buffered reader over `Sock` (SMTP is CRLF-delimited).
struct LineSock {
    sock: Sock,
    buf: Vec<u8>,
}

impl LineSock {
    fn new(sock: Sock) -> LineSock {
        LineSock { sock, buf: Vec::new() }
    }
    /// Read one CRLF-terminated line (without the CRLF). Returns None on EOF.
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            if let Some(pos) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let line: Vec<u8> = self.buf.drain(..pos + 2).collect();
                let s = String::from_utf8_lossy(&line[..pos]).into_owned();
                return Ok(Some(s));
            }
            let mut tmp = [0u8; 4096];
            let n = self.sock.read_some(&mut tmp).await?;
            if n == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                let s = String::from_utf8_lossy(&self.buf).into_owned();
                self.buf.clear();
                return Ok(Some(s));
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }
    async fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.sock.write_all(b).await
    }
}

/// Spawn the scripted stub on its own thread/runtime; returns (port, transcript-handle).
/// The transcript collects every client command line (and the DATA blob, prefixed
/// `DATA-BODY:`). The thread serves EXACTLY ONE connection then exits.
fn spawn_stub(greeting: &str, steps: Vec<Step>) -> (u16, Arc<Mutex<Vec<String>>>) {
    let transcript: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let t2 = transcript.clone();
    let greeting = greeting.to_string();
    let (tx, rx) = mpsc::channel::<u16>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tx.send(port).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut ls = LineSock::new(Sock::Plain(stream));
            // greeting
            ls.write_all(greeting.as_bytes()).await.unwrap();
            for step in steps {
                if let Some(d) = step.stall {
                    tokio::time::sleep(d).await;
                }
                if step.read_data {
                    // read until a lone "." line.
                    let mut body = String::new();
                    loop {
                        match ls.read_line().await.unwrap() {
                            Some(l) if l == "." => break,
                            Some(l) => {
                                body.push_str(&l);
                                body.push_str("\r\n");
                            }
                            None => break,
                        }
                    }
                    t2.lock().unwrap().push(format!("DATA-BODY:{}", body));
                } else {
                    match ls.read_line().await.unwrap() {
                        Some(line) => {
                            if let Some(exp) = &step.expect {
                                assert_eq!(&line, exp, "stub: unexpected client command");
                            }
                            t2.lock().unwrap().push(line);
                        }
                        None => {
                            t2.lock().unwrap().push("<EOF>".into());
                            break;
                        }
                    }
                }
                ls.write_all(step.reply.as_bytes()).await.unwrap();
                if step.upgrade_tls {
                    // Upgrade the server side: take the plain stream, wrap with rustls.
                    let cfg = server_tls_config();
                    let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
                    let Sock::Plain(plain) = ls.sock else { unreachable!() };
                    assert!(ls.buf.is_empty(), "client wrote before TLS upgrade (buffering bug)");
                    let tls = acceptor.accept(plain).await.unwrap();
                    ls = LineSock::new(Sock::Tls(Box::new(tls)));
                }
            }
            // give the client a moment to read the final reply before the socket drops
            tokio::time::sleep(Duration::from_millis(50)).await;
        });
    });

    let port = rx.recv_timeout(Duration::from_secs(5)).expect("stub did not bind");
    (port, transcript)
}

/// Run an `.as` program with the given extra flags; returns (success, stdout, stderr).
fn run_script(src: &str, name: &str, flags: &[&str]) -> (bool, String, String) {
    let file = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&file).unwrap();
    f.write_all(src.as_bytes()).unwrap();
    drop(f);
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("run");
    for fl in flags {
        cmd.arg(fl);
    }
    cmd.arg(&file);
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn transcript(t: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    // Give the stub thread a beat to flush its recorded lines.
    std::thread::sleep(Duration::from_millis(150));
    t.lock().unwrap().clone()
}

/// The test cert as an AScript double-quoted string literal (newlines escaped). Used
/// as `caCert` so the self-signed STARTTLS test cert is trusted by the client. Cert
/// SAN is `localhost`, so the TLS cases connect with `serverName: "localhost"`.
fn cert_literal() -> String {
    format!("\"{}\"", CERT.replace('\n', "\\n"))
}

// ─────────────────────────────────────────────────────────────────────────────
// (a) Happy path + dot-stuffing on the wire
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_happy_path_with_dot_stuffing() {
    // The message body has lines starting with '.' → must be dot-stuffed on the wire.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub greets you\r\n250 SIZE 1000000\r\n"),
        Step::line(Some("MAIL FROM:<alice@example.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<bob@example.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 go ahead\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);

    // Body has a line beginning with "." and a line "..already" — both must be stuffed.
    let src = format!(
        r#"import * as email from "std/email"
let [msg, e1] = email.message({{
  from: "alice@example.com",
  to: "bob@example.com",
  subject: "Hi",
  text: ".leading dot line\n..already two\nnormal",
}})
if (e1 != nil) {{ print("build-err"); exit(1) }}
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
print(`rejected=${{len(res.rejected)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_happy.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
    assert!(out.contains("rejected=0"), "out: {out}");

    let t = transcript(&tr);
    let body = t.iter().find(|l| l.starts_with("DATA-BODY:")).expect("no DATA body recorded");
    // Dot-stuffing: a body line ".leading" becomes "..leading"; "..already" → "...already".
    assert!(body.contains("..leading dot line"), "dot-stuffing missing: {body}");
    assert!(body.contains("...already two"), "double-dot not stuffed: {body}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (b) AUTH PLAIN + AUTH LOGIN base64 (over STARTTLS so auth is permitted)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_auth_plain_base64_over_starttls() {
    // base64 of "\0user\0pass" = AHVzZXIAcGFzcw==
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 STARTTLS\r\n"),
        Step::upgrade("STARTTLS", "220 go ahead\r\n"),
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 AUTH PLAIN LOGIN\r\n"),
        Step::line(Some("AUTH PLAIN AHVzZXIAcGFzcw=="), "235 authenticated\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let ca = cert_literal();
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "localhost", port: {port}, tls: "starttls",
  username: "user", password: "pass", caCert: {ca}, serverName: "localhost" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_auth_plain.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
    let t = transcript(&tr);
    assert!(t.iter().any(|l| l == "AUTH PLAIN AHVzZXIAcGFzcw=="), "AUTH PLAIN base64 wrong: {t:?}");
    // The re-EHLO after STARTTLS must appear (twice EHLO total).
    assert_eq!(t.iter().filter(|l| l.as_str() == "EHLO localhost").count(), 2, "missing re-EHLO: {t:?}");
}

#[test]
fn smtp_auth_login_base64_over_starttls() {
    // AUTH LOGIN: base64("user")=dXNlcg==, base64("pass")=cGFzcw==
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 STARTTLS\r\n"),
        Step::upgrade("STARTTLS", "220 go ahead\r\n"),
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 AUTH LOGIN\r\n"),
        Step::line(Some("AUTH LOGIN"), "334 VXNlcm5hbWU6\r\n"),
        Step::line(Some("dXNlcg=="), "334 UGFzc3dvcmQ6\r\n"),
        Step::line(Some("cGFzcw=="), "235 ok\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let ca = cert_literal();
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "localhost", port: {port}, tls: "starttls",
  username: "user", password: "pass", authMethod: "login", caCert: {ca}, serverName: "localhost" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_auth_login.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
    let t = transcript(&tr);
    assert!(t.iter().any(|l| l == "dXNlcg=="), "AUTH LOGIN user base64 wrong: {t:?}");
    assert!(t.iter().any(|l| l == "cGFzcw=="), "AUTH LOGIN pass base64 wrong: {t:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (c) STARTTLS upgrade + re-EHLO (no auth)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_starttls_upgrade_and_reehlo() {
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 STARTTLS\r\n"),
        Step::upgrade("STARTTLS", "220 go ahead\r\n"),
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let ca = cert_literal();
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "localhost", port: {port}, tls: "starttls",
  caCert: {ca}, serverName: "localhost" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_starttls_ok.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (d) STARTTLS-STRIP DEFENSE — requested but unavailable → Tier-1, NEVER plaintext
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_starttls_strip_defense_no_advertise() {
    // The server does NOT advertise STARTTLS but the client requested tls:"starttls".
    // The client MUST refuse (Tier-1 err), and MUST NOT send MAIL/AUTH in plaintext.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub (no starttls)\r\n"),
        Step::line(None, "221 bye\r\n"), // whatever the client sends next (QUIT or RSET)
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "starttls" }})
if (err == nil) {{ print("SECURITY-FAIL: sent in plaintext"); exit(1) }}
print(`refused: ${{err.message}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_strip_noadv.as", &[]);
    assert!(ok, "script should run (Tier-1 err is recoverable): {out}\n{err}");
    assert!(out.contains("refused:"), "expected refusal, got: {out}");
    assert!(out.to_lowercase().contains("starttls"), "refusal should mention STARTTLS: {out}");
    // The transcript must NOT contain a MAIL FROM (no plaintext leakage).
    let t = transcript(&tr);
    assert!(!t.iter().any(|l| l.starts_with("MAIL FROM")), "SECURITY: MAIL sent in plaintext: {t:?}");
}

#[test]
fn smtp_starttls_strip_defense_handshake_fails() {
    // The server advertises STARTTLS and replies 220, but then sends garbage instead
    // of a TLS handshake → the upgrade FAILS → Tier-1, never plaintext fallback.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 STARTTLS\r\n"),
        // Reply 220 then DON'T upgrade — send garbage bytes; the client's TLS handshake fails.
        Step::line(Some("STARTTLS"), "220 go ahead\r\nGARBAGE-NOT-TLS\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "starttls" }})
if (err == nil) {{ print("SECURITY-FAIL: continued after failed upgrade"); exit(1) }}
print(`refused: ${{err.message}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_strip_handshake.as", &[]);
    assert!(ok, "script should run (Tier-1): {out}\n{err}");
    assert!(out.contains("refused:"), "expected refusal: {out}");
    let t = transcript(&tr);
    assert!(!t.iter().any(|l| l.starts_with("MAIL FROM")), "SECURITY: MAIL after failed upgrade: {t:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (e) Partial RCPT rejection → accepted + rejected both populated
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_partial_rcpt_rejection() {
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<good@d.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<bad@d.com>"), "550 no such user\r\n"),
        Step::line(Some("RCPT TO:<good2@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com",
  to: ["good@d.com", "bad@d.com", "good2@d.com"], subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
print(`rejected=${{len(res.rejected)}}`)
print(`first-rejected=${{res.rejected[0].address}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_partial_rcpt.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=2"), "out: {out}");
    assert!(out.contains("rejected=1"), "out: {out}");
    assert!(out.contains("first-rejected=bad@d.com"), "out: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (f) Multi-line replies (continuation '-' vs final ' ')
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_multiline_reply_parsing() {
    // The greeting + EHLO reply both span multiple lines. The client must consume the
    // whole continuation and read the FINAL line's code.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub line one\r\n250-stub line two\r\n250 stub final\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, _tr) = spawn_stub("220-multiline greeting\r\n220 ready\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none" }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_multiline.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (g) 5xx at MAIL FROM → Tier-1 carrying the server's text
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_5xx_at_mail_from_is_tier1() {
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "550 sender rejected by policy\r\n"),
        Step::line(None, "221 bye\r\n"), // client QUITs
    ];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none" }})
if (err == nil) {{ print("FAIL: 5xx not surfaced"); exit(1) }}
print(`err: ${{err.message}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_5xx_mail.as", &[]);
    assert!(ok, "script should run (Tier-1): {out}\n{err}");
    assert!(out.contains("err:"), "out: {out}");
    assert!(out.contains("sender rejected by policy"), "server text not carried: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (h) AUTH WITHOUT TLS → Tier-2 unless allowInsecureAuth:true
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_auth_without_tls_is_tier2() {
    // tls:"none" + username/password but NO allowInsecureAuth → Tier-2 panic BEFORE
    // any credential is sent. The script does NOT recover → process aborts (exit != 0).
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(None, "221 bye\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none",
  username: "user", password: "pass" }})
print("should-not-reach")
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_auth_no_tls.as", &[]);
    assert!(!ok, "AUTH without TLS must be a Tier-2 panic (non-zero exit): {out}\n{err}");
    assert!(!out.contains("should-not-reach"), "panic did not abort: {out}");
    let combined = format!("{out}{err}");
    assert!(
        combined.to_lowercase().contains("plaintext") || combined.to_lowercase().contains("insecure"),
        "panic message should explain plaintext-auth refusal: {combined}"
    );
    // No credentials must have crossed the wire.
    let t = transcript(&tr);
    assert!(!t.iter().any(|l| l.starts_with("AUTH")), "SECURITY: AUTH sent in plaintext: {t:?}");
}

#[test]
fn smtp_auth_without_tls_allowed_with_optin() {
    // allowInsecureAuth:true → permitted (the loud opt-out). AUTH PLAIN goes over plaintext.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250-stub\r\n250 AUTH PLAIN\r\n"),
        Step::line(Some("AUTH PLAIN AHVzZXIAcGFzcw=="), "235 ok\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none",
  username: "user", password: "pass", allowInsecureAuth: true }})
if (err != nil) {{ print(`send-err: ${{err.message}}`); exit(1) }}
print(`accepted=${{len(res.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_auth_optin.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("accepted=1"), "out: {out}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (i) Timeout honored — stub stalls → Tier-1 timeout, no hang
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_timeout_when_server_stalls() {
    // The stub stalls 10s before replying to EHLO; the client's 1s timeout fires.
    let steps = vec![Step::stall(10, "250 stub\r\n")];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [msg, _] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "x", text: "body" }})
let [res, err] = email.send(msg, {{ host: "127.0.0.1", port: {port}, tls: "none", timeout: 1000 }})
if (err == nil) {{ print("FAIL: no timeout"); exit(1) }}
print(`err: ${{err.message}}`)
"#
    );
    let start = std::time::Instant::now();
    let (ok, out, err) = run_script(&src, "smtp_timeout.as", &[]);
    let elapsed = start.elapsed();
    assert!(ok, "script should run (Tier-1 timeout): {out}\n{err}");
    assert!(out.to_lowercase().contains("timed out") || out.to_lowercase().contains("timeout"),
        "expected timeout message: {out}");
    assert!(elapsed < Duration::from_secs(8), "did not time out promptly: {elapsed:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// (j) WIRE-LAYER CRLF DEFENSE — a hand-built msg object still can't inject
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_wire_layer_crlf_revalidation() {
    // Construct a RAW msg object (bypassing the builder's reject_crlf) whose envelope
    // `to` carries a CRLF smuggling a RCPT TO command. `send` MUST re-validate at the
    // wire layer and refuse (Tier-2). The script does NOT recover → non-zero exit.
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(None, "221 bye\r\n"),
    ];
    let (port, tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let evil = {{
  __email: true,
  raw: "From: a@b.com\r\nTo: c@d.com\r\nSubject: x\r\n\r\nbody",
  envelope: {{
    from: "a@b.com",
    to: ["c@d.com\r\nRCPT TO:<evil@x>"],
    cc: [],
    bcc: [],
  }},
}}
let [res, err] = email.send(evil, {{ host: "127.0.0.1", port: {port}, tls: "none" }})
print("should-not-reach")
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_wire_crlf.as", &[]);
    assert!(!ok, "wire-layer CRLF must be a Tier-2 panic (non-zero exit): {out}\n{err}");
    assert!(!out.contains("should-not-reach"), "injection not caught: {out}");
    let combined = format!("{out}{err}");
    assert!(combined.to_lowercase().contains("injection") || combined.to_lowercase().contains("control"),
        "panic should be the header-injection guard: {combined}");
    let t = transcript(&tr);
    assert!(!t.iter().any(|l| l.contains("RCPT TO:<evil")), "SECURITY: injected RCPT reached wire: {t:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// connect()/client.send() reuse
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn smtp_connect_reuse_client() {
    let steps = vec![
        Step::line(Some("EHLO localhost"), "250 stub\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<c@d.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("MAIL FROM:<a@b.com>"), "250 ok\r\n"),
        Step::line(Some("RCPT TO:<e@f.com>"), "250 ok\r\n"),
        Step::line(Some("DATA"), "354 ok\r\n"),
        Step::data("250 queued\r\n"),
        Step::line(Some("QUIT"), "221 bye\r\n"),
    ];
    let (port, _tr) = spawn_stub("220 stub ESMTP\r\n", steps);
    let src = format!(
        r#"import * as email from "std/email"
let [client, err] = email.connect({{ host: "127.0.0.1", port: {port}, tls: "none" }})
if (err != nil) {{ print(`connect-err: ${{err.message}}`); exit(1) }}
let [m1, me1] = email.message({{ from: "a@b.com", to: "c@d.com", subject: "1", text: "one" }})
let [r1, e1] = client.send(m1)
if (e1 != nil) {{ print(`send1-err: ${{e1.message}}`); exit(1) }}
let [m2, me2] = email.message({{ from: "a@b.com", to: "e@f.com", subject: "2", text: "two" }})
let [r2, e2] = client.send(m2)
if (e2 != nil) {{ print(`send2-err: ${{e2.message}}`); exit(1) }}
client.close()
print(`ok=${{len(r1.accepted) + len(r2.accepted)}}`)
"#
    );
    let (ok, out, err) = run_script(&src, "smtp_connect_reuse.as", &[]);
    assert!(ok, "script failed: {out}\n{err}");
    assert!(out.contains("ok=2"), "out: {out}");
}
