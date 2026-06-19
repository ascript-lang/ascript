//! BATT B6 §8.2 — the hand-rolled SMTP CLIENT (the network half of `std/email`).
//!
//! A small RFC 5321 state machine over tokio TCP + the shared `tls` plumbing
//! (`crate::stdlib::tls::client_config`). The threat model this file defends:
//!
//!   1. **STARTTLS stripping (silent downgrade).** A `tls:"starttls"` request that
//!      cannot complete the upgrade — the server does not advertise `STARTTLS`, or
//!      the `220` is followed by a failed handshake — MUST abort (Tier-1). The client
//!      NEVER falls back to plaintext and NEVER sends `MAIL`/`AUTH` over an
//!      unencrypted socket once TLS was requested. Enforced by [`SmtpSession::secure`]:
//!      it is `false` until a real TLS upgrade (STARTTLS or implicit) succeeds, and
//!      [`SmtpSession::ensure_starttls`] returns an `Err` (→ Tier-1) the instant the
//!      upgrade is impossible.
//!   2. **Credential exposure over plaintext.** `AUTH` over a non-TLS connection is a
//!      programmer error → **Tier-2 panic** BEFORE any credential touches the wire,
//!      unless the caller passes the loud `allowInsecureAuth: true`.
//!   3. **Wire-layer SMTP injection.** Even a hand-built message object that bypassed
//!      the builder's `reject_crlf` is RE-VALIDATED here (`from`/recipients) before a
//!      single `MAIL`/`RCPT`/`DATA` byte is written — a CRLF in an envelope address
//!      is a Tier-2 panic, the last line of defense.
//!
//! Every SERVER interaction (a 4xx/5xx reply, a closed socket, a timeout) is Tier-1
//! `[result, err]`. Misuse (bad opts type, AUTH-without-TLS, an injected address) is
//! Tier-2. The reply-line read is BOUNDED (`MAX_REPLY_BYTES`) so a hostile server
//! cannot OOM the host with an endless line.

use super::reject_crlf;
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use base64::Engine as _;
use std::rc::Rc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Default per-command + connect budget (overridable via `timeout` ms).
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Cap a single reply line so a hostile server can't OOM us with an endless line.
const MAX_REPLY_BYTES: usize = 64 * 1024;

/// The transport: a plaintext TCP socket or a TLS-wrapped one. We unify them behind
/// a single read/write surface so the state machine doesn't branch per command.
enum Transport {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<tokio_rustls::client::TlsStream<TcpStream>>>),
}

impl Transport {
    async fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        match self {
            Transport::Plain(s) => s.get_mut().write_all(b).await,
            Transport::Tls(s) => s.get_mut().write_all(b).await,
        }
    }
    async fn read_byte(&mut self) -> std::io::Result<Option<u8>> {
        let mut b = [0u8; 1];
        let n = match self {
            Transport::Plain(s) => s.read(&mut b).await?,
            Transport::Tls(s) => s.read(&mut b).await?,
        };
        Ok(if n == 0 { None } else { Some(b[0]) })
    }
}

/// One parsed SMTP reply: the 3-digit code + the joined text of all lines.
struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn is_positive(&self) -> bool {
        (200..400).contains(&self.code)
    }
}

/// The live SMTP session: the transport + whether the channel is encrypted + the
/// negotiated server capabilities (from the last EHLO). `secure` is the keystone of
/// the STARTTLS-strip defense — it is `true` ONLY after a real TLS upgrade.
pub(crate) struct SmtpClientState {
    transport: Transport,
    /// True iff the channel is encrypted (implicit TLS or a completed STARTTLS).
    secure: bool,
    /// Uppercased EHLO keywords advertised by the server (e.g. "STARTTLS", "AUTH").
    ehlo_keywords: Vec<String>,
    /// The AUTH mechanisms the server offered (the tokens after `AUTH`).
    auth_mechs: Vec<String>,
    /// Per-command timeout.
    timeout: Duration,
    /// The host (for the EHLO domain + SNI on a STARTTLS upgrade).
    host: String,
}

impl SmtpClientState {
    async fn read_reply(&mut self) -> Result<Reply, String> {
        let fut = async {
            let mut code: Option<u16> = None;
            let mut text = String::new();
            let mut total = 0usize;
            loop {
                // Read one CRLF-terminated line, bounded.
                let mut line = Vec::new();
                loop {
                    match self.transport.read_byte().await.map_err(|e| e.to_string())? {
                        None => {
                            if line.is_empty() && text.is_empty() {
                                return Err("connection closed by server".to_string());
                            }
                            break;
                        }
                        Some(b'\n') => break,
                        Some(b'\r') => { /* swallow; the \n ends the line */ }
                        Some(b) => {
                            line.push(b);
                            total += 1;
                            if total > MAX_REPLY_BYTES {
                                return Err("server reply exceeded the maximum size".to_string());
                            }
                        }
                    }
                }
                let line_str = String::from_utf8_lossy(&line).into_owned();
                // A reply line is `NNN<sep><text>` where sep is '-' (continuation) or
                // ' ' (final). Lines shorter than 3 chars are malformed.
                if line_str.len() < 3 {
                    return Err(format!("malformed SMTP reply line: {:?}", line_str));
                }
                let (code_str, rest) = line_str.split_at(3);
                let this_code: u16 = code_str
                    .parse()
                    .map_err(|_| format!("malformed SMTP reply code: {:?}", code_str))?;
                if let Some(c) = code {
                    if c != this_code {
                        // RFC 5321 §4.2.1 — all lines of one reply share a code.
                        return Err("inconsistent SMTP reply codes".to_string());
                    }
                } else {
                    code = Some(this_code);
                }
                let sep = rest.chars().next();
                let body = rest.get(1..).unwrap_or("");
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(body.trim());
                match sep {
                    Some('-') => continue,      // continuation; read another line
                    _ => break,                 // ' ' (final) or empty → done
                }
            }
            Ok(Reply { code: code.unwrap(), text })
        };
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err("timed out waiting for server reply".to_string()),
        }
    }

    async fn write_line(&mut self, line: &str) -> Result<(), String> {
        let mut buf = line.as_bytes().to_vec();
        buf.extend_from_slice(b"\r\n");
        match tokio::time::timeout(self.timeout, self.transport.write_all(&buf)).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("timed out writing to server".to_string()),
        }
    }

    /// Write `\r\n`-less raw bytes (used for the DATA payload which we frame ourselves).
    async fn write_raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        match tokio::time::timeout(self.timeout, self.transport.write_all(bytes)).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("timed out writing to server".to_string()),
        }
    }

    /// Send a command and read its reply.
    async fn command(&mut self, line: &str) -> Result<Reply, String> {
        self.write_line(line).await?;
        self.read_reply().await
    }

    /// Send EHLO and record the advertised capabilities. The EHLO text body lines
    /// (after the first) are the capability keywords.
    async fn ehlo(&mut self) -> Result<(), String> {
        // Re-parse the EHLO reply line-by-line for capabilities. `read_reply` joins
        // lines with spaces, so we re-derive keywords by splitting on whitespace and
        // taking the leading token of each advertised extension.
        self.write_line(&format!("EHLO {}", ehlo_domain(&self.host)))
            .await?;
        let reply = self.read_ehlo_caps().await?;
        if !reply.is_positive() {
            return Err(format!("EHLO rejected: {} {}", reply.code, reply.text));
        }
        Ok(())
    }

    /// Like `read_reply` but ALSO captures each continuation line as a capability
    /// keyword (STARTTLS / AUTH / SIZE / …). Bounded identically.
    async fn read_ehlo_caps(&mut self) -> Result<Reply, String> {
        let fut = async {
            let mut code: Option<u16> = None;
            let mut joined = String::new();
            let mut keywords: Vec<String> = Vec::new();
            let mut auth_mechs: Vec<String> = Vec::new();
            let mut total = 0usize;
            let mut first = true;
            loop {
                let mut line = Vec::new();
                loop {
                    match self.transport.read_byte().await.map_err(|e| e.to_string())? {
                        None => {
                            if line.is_empty() && joined.is_empty() {
                                return Err("connection closed by server".to_string());
                            }
                            break;
                        }
                        Some(b'\n') => break,
                        Some(b'\r') => {}
                        Some(b) => {
                            line.push(b);
                            total += 1;
                            if total > MAX_REPLY_BYTES {
                                return Err("server reply exceeded the maximum size".to_string());
                            }
                        }
                    }
                }
                let line_str = String::from_utf8_lossy(&line).into_owned();
                if line_str.len() < 3 {
                    return Err(format!("malformed SMTP reply line: {:?}", line_str));
                }
                let (code_str, rest) = line_str.split_at(3);
                let this_code: u16 = code_str
                    .parse()
                    .map_err(|_| format!("malformed SMTP reply code: {:?}", code_str))?;
                if let Some(c) = code {
                    if c != this_code {
                        return Err("inconsistent SMTP reply codes".to_string());
                    }
                } else {
                    code = Some(this_code);
                }
                let sep = rest.chars().next();
                let body = rest.get(1..).unwrap_or("").trim();
                // The FIRST line is the greeting/domain, NOT a capability.
                if !first && !body.is_empty() {
                    let mut toks = body.split_whitespace();
                    if let Some(kw) = toks.next() {
                        let kw_up = kw.to_ascii_uppercase();
                        if kw_up == "AUTH" {
                            for m in toks {
                                auth_mechs.push(m.to_ascii_uppercase());
                            }
                        }
                        keywords.push(kw_up);
                    }
                }
                first = false;
                if !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(body);
                match sep {
                    Some('-') => continue,
                    _ => break,
                }
            }
            self.ehlo_keywords = keywords;
            self.auth_mechs = auth_mechs;
            Ok(Reply { code: code.unwrap(), text: joined })
        };
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err("timed out waiting for server reply".to_string()),
        }
    }

    fn advertises(&self, keyword: &str) -> bool {
        self.ehlo_keywords.iter().any(|k| k == keyword)
    }
}

/// The EHLO domain: a bare hostname or IP. RFC says a FQDN or an address literal; for
/// an IP we use `localhost` (the test/dev convention) to avoid bracket-literal parsing
/// edge cases on the server. A real hostname is sent verbatim.
fn ehlo_domain(host: &str) -> String {
    if host.parse::<std::net::IpAddr>().is_ok() {
        "localhost".to_string()
    } else {
        host.to_string()
    }
}

/// The TLS mode requested by the caller.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TlsMode {
    Starttls,
    Implicit,
    None,
}

/// The AUTH mechanism.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMethod {
    Plain,
    Login,
}

/// The parsed connection options.
struct ConnectOpts {
    host: String,
    port: u16,
    tls: TlsMode,
    username: Option<String>,
    password: Option<String>,
    auth_method: AuthMethod,
    allow_insecure_auth: bool,
    ca_cert: Option<String>,
    /// Override the TLS SNI / cert-verification name (defaults to `host`).
    server_name: Option<String>,
    timeout: Duration,
}

/// Parse + type-check `opts` (Tier-2 on a wrong type / bad enum value). The port
/// default follows `tls`: 587 (starttls) / 465 (implicit) / 25 (none).
fn parse_connect_opts(opts: &Value, span: Span) -> Result<ConnectOpts, Control> {
    let ValueKind::Object(o) = opts.kind() else {
        return Err(AsError::at(
            "email connection opts must be an object {host, port?, tls?, …}",
            span,
        )
        .into());
    };
    let host = match o.get("host") {
        Some(v) => want_str(&v, span, "email opts.host")?,
        None => return Err(AsError::at("email opts: 'host' is required", span).into()),
    };
    let tls = match o.get("tls") {
        None => TlsMode::Starttls,
        Some(v) if matches!(v.kind(), ValueKind::Nil) => TlsMode::Starttls,
        Some(v) => match want_str(&v, span, "email opts.tls")?.as_str() {
            "starttls" => TlsMode::Starttls,
            "implicit" => TlsMode::Implicit,
            "none" => TlsMode::None,
            other => {
                return Err(AsError::at(
                    format!("email opts.tls must be \"starttls\", \"implicit\", or \"none\", got {:?}", other),
                    span,
                )
                .into())
            }
        },
    };
    let port = match o.get("port") {
        None => default_port(tls),
        Some(v) if matches!(v.kind(), ValueKind::Nil) => default_port(tls),
        Some(v) => match v.kind() {
            ValueKind::Int(n) if (1..=65535).contains(&n) => n as u16,
            _ => {
                return Err(AsError::at(
                    format!("email opts.port must be an int 1..=65535, got {}", crate::interp::type_name(&v)),
                    span,
                )
                .into())
            }
        },
    };
    let username = opt_str(o, "username", span)?;
    let password = opt_str(o, "password", span)?;
    let auth_method = match o.get("authMethod") {
        None => AuthMethod::Plain,
        Some(v) if matches!(v.kind(), ValueKind::Nil) => AuthMethod::Plain,
        Some(v) => match want_str(&v, span, "email opts.authMethod")?.as_str() {
            "plain" => AuthMethod::Plain,
            "login" => AuthMethod::Login,
            other => {
                return Err(AsError::at(
                    format!("email opts.authMethod must be \"plain\" or \"login\", got {:?}", other),
                    span,
                )
                .into())
            }
        },
    };
    let allow_insecure_auth = match o.get("allowInsecureAuth") {
        None => false,
        Some(v) => v.is_truthy(),
    };
    let ca_cert = opt_str(o, "caCert", span)?;
    let server_name = opt_str(o, "serverName", span)?;
    let timeout = match o.get("timeout") {
        None => Duration::from_millis(DEFAULT_TIMEOUT_MS),
        Some(v) if matches!(v.kind(), ValueKind::Nil) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
        Some(v) => match v.kind() {
            ValueKind::Int(n) if n > 0 => Duration::from_millis(n as u64),
            _ => {
                return Err(AsError::at(
                    format!("email opts.timeout must be a positive int (ms), got {}", crate::interp::type_name(&v)),
                    span,
                )
                .into())
            }
        },
    };
    Ok(ConnectOpts {
        host,
        port,
        tls,
        username,
        password,
        auth_method,
        allow_insecure_auth,
        ca_cert,
        server_name,
        timeout,
    })
}

fn default_port(tls: TlsMode) -> u16 {
    match tls {
        TlsMode::Starttls => 587,
        TlsMode::Implicit => 465,
        TlsMode::None => 25,
    }
}

fn want_str(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        _ => Err(AsError::at(format!("{} must be a string, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

fn opt_str(
    o: &gcmodule::Cc<crate::value::ObjectCell>,
    key: &str,
    span: Span,
) -> Result<Option<String>, Control> {
    match o.get(key) {
        None => Ok(None),
        Some(v) if matches!(v.kind(), ValueKind::Nil) => Ok(None),
        Some(v) => Ok(Some(want_str(&v, span, &format!("email opts.{}", key))?)),
    }
}

/// The envelope (from + recipients) extracted from a message value, RE-VALIDATED
/// against `reject_crlf` at the wire layer (defense even for a hand-built object).
struct Envelope {
    from: String,
    recipients: Vec<String>,
    raw: String,
}

/// Extract + re-validate the envelope from a message value (the tagged
/// `{__email, raw, envelope}` object). A CRLF/NUL anywhere in `from` or a recipient
/// is a Tier-2 panic HERE — even if the object bypassed the builder.
fn extract_envelope(msg: &Value, span: Span) -> Result<Envelope, Control> {
    let ValueKind::Object(o) = msg.kind() else {
        return Err(AsError::at(
            format!("email.send: message must be a built email object, got {}", crate::interp::type_name(msg)),
            span,
        )
        .into());
    };
    let raw = match o.get("raw") {
        Some(v) => want_str(&v, span, "email message.raw")?,
        None => return Err(AsError::at("email.send: message is missing 'raw'", span).into()),
    };
    let env = match o.get("envelope") {
        Some(v) => v,
        None => return Err(AsError::at("email.send: message is missing 'envelope'", span).into()),
    };
    let ValueKind::Object(eo) = env.kind() else {
        return Err(AsError::at("email.send: message envelope must be an object", span).into());
    };
    let from = match eo.get("from") {
        Some(v) => want_str(&v, span, "email envelope.from")?,
        None => return Err(AsError::at("email.send: envelope is missing 'from'", span).into()),
    };
    // WIRE-LAYER injection defense: re-reject CRLF/NUL on every envelope address.
    reject_crlf("from", &from, span)?;

    let mut recipients = Vec::new();
    for key in ["to", "cc", "bcc"] {
        if let Some(list) = eo.get(key) {
            if let ValueKind::Array(a) = list.kind() {
                for (i, item) in a.borrow().iter().enumerate() {
                    let addr = want_str(item, span, &format!("email envelope.{}", key))?;
                    reject_crlf(&format!("{}[{}]", key, i), &addr, span)?;
                    recipients.push(addr);
                }
            }
        }
    }
    if recipients.is_empty() {
        return Err(AsError::at("email.send: envelope has no recipients", span).into());
    }
    Ok(Envelope { from, recipients, raw })
}

/// Dot-stuff a DATA payload per RFC 5321 §4.5.2: a line beginning with '.' gets a
/// leading '.' prepended; the payload is terminated by `\r\n.\r\n`. Line endings are
/// normalized to CRLF.
fn dot_stuff(raw: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 64 + 8);
    // Split on \n, stripping a trailing \r so we can re-emit canonical CRLF.
    let mut lines = raw.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('.') {
            out.push(b'.'); // the stuffed extra dot
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
        let _ = lines.peek();
    }
    // Terminator.
    out.extend_from_slice(b".\r\n");
    out
}

impl Interp {
    /// BATT B6 §8.2 — dispatch `email.send` / `email.connect` (the Net-gated SMTP
    /// client). The cap gate already fired at `call_stdlib` (`required_cap("email",
    /// "send"|"connect") == Net`).
    #[cfg(feature = "email")]
    pub(crate) async fn call_email_async(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "send" => {
                let msg = crate::stdlib::arg(args, 0);
                let opts = crate::stdlib::arg(args, 1);
                self.email_send(&msg, &opts, span).await
            }
            "connect" => {
                let opts = crate::stdlib::arg(args, 0);
                self.email_connect(&opts, span).await
            }
            _ => Err(AsError::at(format!("std/email has no async function '{}'", func), span).into()),
        }
    }

    /// `email.connect(opts) -> [client, err]`. Opens + greets + EHLOs + (STARTTLS) +
    /// (AUTH); registers a reusable `SmtpClient` handle. Misuse → Tier-2; any server/
    /// network failure → Tier-1.
    async fn email_connect(&self, opts: &Value, span: Span) -> Result<Value, Control> {
        let opts = parse_connect_opts(opts, span)?;
        // AUTH-without-TLS is a Tier-2 PROGRAMMER ERROR — raised BEFORE any socket I/O
        // so credentials can never reach a plaintext wire.
        guard_plaintext_auth(&opts, span)?;
        self.check_net_host(&opts.host, span)?;
        match self.smtp_open(opts).await {
            Ok(state) => {
                let handle = self.register_resource(
                    NativeKind::SmtpClient,
                    indexmap::IndexMap::new(),
                    ResourceState::SmtpClient(Box::new(state)),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
        }
    }

    /// `email.send(msg, opts) -> [{accepted, rejected}, err]`. A one-shot: connect,
    /// send one message, QUIT. Errors are Tier-1 (server) / Tier-2 (misuse).
    async fn email_send(&self, msg: &Value, opts: &Value, span: Span) -> Result<Value, Control> {
        let opts = parse_connect_opts(opts, span)?;
        guard_plaintext_auth(&opts, span)?;
        // WIRE-LAYER injection defense + envelope extraction (Tier-2 on a smuggled CRLF)
        // happens BEFORE we open the socket.
        let env = extract_envelope(msg, span)?;
        self.check_net_host(&opts.host, span)?;
        let mut state = match self.smtp_open(opts).await {
            Ok(s) => s,
            Err(msg) => return Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
        };
        let result = smtp_transact(&mut state, &env).await;
        // Best-effort QUIT, then drop the socket.
        let _ = state.command("QUIT").await;
        match result {
            Ok(v) => Ok(make_pair(v, Value::nil())),
            Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
        }
    }

    /// Open the connection: TCP connect, (implicit TLS), greeting, EHLO, (STARTTLS +
    /// re-EHLO), (AUTH). Returns the ready session or a Tier-1 error string.
    async fn smtp_open(&self, opts: ConnectOpts) -> Result<SmtpClientState, String> {
        let addr = format!("{}:{}", opts.host, opts.port);
        let connect = TcpStream::connect(&addr);
        let tcp = match tokio::time::timeout(opts.timeout, connect).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("connect to {} failed: {}", addr, e)),
            Err(_) => return Err(format!("connect to {} timed out", addr)),
        };

        let (transport, secure) = if opts.tls == TlsMode::Implicit {
            // Implicit TLS (port 465): handshake FIRST, before any SMTP byte.
            let tls = self.tls_upgrade(tcp, &opts).await?;
            (Transport::Tls(Box::new(BufReader::new(tls))), true)
        } else {
            (Transport::Plain(BufReader::new(tcp)), false)
        };

        let mut state = SmtpClientState {
            transport,
            secure,
            ehlo_keywords: Vec::new(),
            auth_mechs: Vec::new(),
            timeout: opts.timeout,
            host: opts.host.clone(),
        };

        // Read the greeting (220).
        let greeting = state.read_reply().await?;
        if greeting.code != 220 {
            return Err(format!("unexpected greeting: {} {}", greeting.code, greeting.text));
        }

        // EHLO.
        state.ehlo().await?;

        // STARTTLS upgrade (the strip defense lives here).
        if opts.tls == TlsMode::Starttls {
            state = self.starttls_upgrade(state, &opts).await?;
        }

        // AUTH (only after a secure channel, unless allowInsecureAuth). The
        // plaintext-auth Tier-2 guard fired pre-connect; this is a defense-in-depth
        // re-check against the LIVE session `secure` flag — so even an implementation
        // bug in the STARTTLS path can never let credentials onto a plaintext wire.
        if opts.username.is_some() || opts.password.is_some() {
            if !state.secure && !opts.allow_insecure_auth {
                return Err(
                    "refusing to authenticate over an unencrypted connection (the channel did not \
                     become secure) — credentials were NOT sent"
                        .to_string(),
                );
            }
            smtp_auth(&mut state, &opts).await?;
        }

        Ok(state)
    }

    /// Perform the STARTTLS upgrade. **THE STARTTLS-STRIP DEFENSE:** if the server did
    /// not advertise STARTTLS, or the `STARTTLS` command is refused, or the TLS
    /// handshake fails — return `Err` (→ Tier-1). The caller NEVER continues in
    /// plaintext after `tls:"starttls"` was requested.
    async fn starttls_upgrade(
        &self,
        mut state: SmtpClientState,
        opts: &ConnectOpts,
    ) -> Result<SmtpClientState, String> {
        if !state.advertises("STARTTLS") {
            return Err(
                "STARTTLS requested but the server does not advertise it — refusing to send mail \
                 over an unencrypted connection (set tls:\"none\" to allow plaintext)"
                    .to_string(),
            );
        }
        let reply = state.command("STARTTLS").await?;
        if reply.code != 220 {
            return Err(format!(
                "STARTTLS refused by server ({} {}) — refusing plaintext fallback",
                reply.code, reply.text
            ));
        }
        // Upgrade the underlying TcpStream. We must own the raw TcpStream — pull it out
        // of the BufReader (any buffered bytes would be a protocol violation: a correct
        // server sends nothing between the 220 and the handshake).
        let tcp = match state.transport {
            Transport::Plain(reader) => {
                if !reader.buffer().is_empty() {
                    return Err(
                        "server sent data before the TLS handshake — refusing STARTTLS upgrade"
                            .to_string(),
                    );
                }
                reader.into_inner()
            }
            Transport::Tls(_) => return Err("STARTTLS on an already-secure channel".to_string()),
        };
        let tls = self.tls_upgrade(tcp, opts).await?;
        state.transport = Transport::Tls(Box::new(BufReader::new(tls)));
        state.secure = true;
        // RE-EHLO over the now-encrypted channel (RFC 5321 §3.2).
        state.ehlo().await?;
        Ok(state)
    }

    /// Run the rustls client handshake over `tcp`, bounded by the timeout. A bad
    /// caCert / handshake failure is a Tier-1 string (NEVER a plaintext fallback).
    async fn tls_upgrade(
        &self,
        tcp: TcpStream,
        opts: &ConnectOpts,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
        let cfg = crate::stdlib::tls::client_config(opts.ca_cert.as_deref(), &[])
            .map_err(|e| format!("TLS config error: {}", e))?;
        let name = opts.server_name.clone().unwrap_or_else(|| opts.host.clone());
        let sni = tokio_rustls::rustls::pki_types::ServerName::try_from(name.clone())
            .map_err(|_| format!("invalid TLS server name '{}'", name))?;
        let connector = tokio_rustls::TlsConnector::from(cfg);
        match tokio::time::timeout(opts.timeout, connector.connect(sni, tcp)).await {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(format!("TLS handshake failed: {}", e)),
            Err(_) => Err("TLS handshake timed out".to_string()),
        }
    }

    /// Dispatch a method on a live `SmtpClient` handle (`client.send(msg)` /
    /// `client.close()`). Take-out-across-await so the `resources` borrow is never held
    /// across a socket round-trip.
    #[cfg(feature = "email")]
    pub(crate) async fn call_smtp_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "send" => {
                let msg = crate::stdlib::arg(&args, 0);
                // Re-validate the envelope at the wire layer BEFORE taking the resource
                // (so an injection on a reused client is a Tier-2 panic, not a half-sent
                // transaction).
                let env = extract_envelope(&msg, span)?;
                let mut state = match self.take_resource(id) {
                    Some(ResourceState::SmtpClient(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str("email client is closed".to_string())),
                        ));
                    }
                };
                let result = smtp_transact(&mut state, &env).await;
                // Keep the client open for reuse (unless the connection broke).
                match &result {
                    Ok(_) => self.return_resource(id, ResourceState::SmtpClient(state)),
                    Err(_) => { /* connection is suspect; drop it */ }
                }
                match result {
                    Ok(v) => Ok(make_pair(v, Value::nil())),
                    Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
                }
            }
            "close" => {
                // Best-effort QUIT, then drop.
                if let Some(ResourceState::SmtpClient(mut state)) = self.take_resource(id) {
                    let _ = state.command("QUIT").await;
                }
                Ok(Value::nil())
            }
            other => {
                Err(AsError::at(format!("smtpClient has no method '{}'", other), span).into())
            }
        }
    }
}

/// AUTH-without-TLS guard (Tier-2): a username/password over a non-TLS connection is a
/// programmer error unless `allowInsecureAuth: true`. Raised BEFORE any socket I/O.
fn guard_plaintext_auth(opts: &ConnectOpts, span: Span) -> Result<(), Control> {
    let has_creds = opts.username.is_some() || opts.password.is_some();
    let will_be_secure = opts.tls != TlsMode::None;
    if has_creds && !will_be_secure && !opts.allow_insecure_auth {
        return Err(AsError::at(
            "email: refusing to send credentials over a plaintext connection (tls:\"none\" with \
             username/password) — use tls:\"starttls\"/\"implicit\", or pass allowInsecureAuth:true \
             to override (NOT recommended)",
            span,
        )
        .into());
    }
    Ok(())
}

/// Authenticate over the (now possibly-secure) session. A runtime AUTH failure (bad
/// credentials → 535) is a Tier-1 error string. The plaintext-auth Tier-2 guard
/// already fired pre-connect; here we only need the per-mechanism dialogue.
async fn smtp_auth(state: &mut SmtpClientState, opts: &ConnectOpts) -> Result<(), String> {
    let user = opts.username.clone().unwrap_or_default();
    let pass = opts.password.clone().unwrap_or_default();
    let b64 = base64::engine::general_purpose::STANDARD;
    // If the server advertised AUTH mechanisms at all, the requested one must be among
    // them — refuse a mechanism the server didn't offer rather than blindly trying it
    // (a small interoperability guard; an empty `auth_mechs` means the server gave no
    // AUTH list, so we attempt the requested mechanism as-is).
    let wanted = match opts.auth_method {
        AuthMethod::Plain => "PLAIN",
        AuthMethod::Login => "LOGIN",
    };
    if !state.auth_mechs.is_empty() && !state.auth_mechs.iter().any(|m| m == wanted) {
        return Err(format!(
            "server does not offer AUTH {} (advertised: {})",
            wanted,
            state.auth_mechs.join(", ")
        ));
    }
    match opts.auth_method {
        AuthMethod::Plain => {
            // AUTH PLAIN base64(\0user\0pass).
            let mut raw = Vec::new();
            raw.push(0u8);
            raw.extend_from_slice(user.as_bytes());
            raw.push(0u8);
            raw.extend_from_slice(pass.as_bytes());
            let token = b64.encode(&raw);
            let reply = state.command(&format!("AUTH PLAIN {}", token)).await?;
            if reply.code != 235 {
                return Err(format!("authentication failed: {} {}", reply.code, reply.text));
            }
        }
        AuthMethod::Login => {
            // AUTH LOGIN → 334 (username prompt) → base64(user) → 334 (password) →
            // base64(pass) → 235.
            let r1 = state.command("AUTH LOGIN").await?;
            if r1.code != 334 {
                return Err(format!("AUTH LOGIN refused: {} {}", r1.code, r1.text));
            }
            let r2 = state.command(&b64.encode(user.as_bytes())).await?;
            if r2.code != 334 {
                return Err(format!("AUTH LOGIN username rejected: {} {}", r2.code, r2.text));
            }
            let r3 = state.command(&b64.encode(pass.as_bytes())).await?;
            if r3.code != 235 {
                return Err(format!("authentication failed: {} {}", r3.code, r3.text));
            }
        }
    }
    Ok(())
}

/// Drive one message transaction: MAIL FROM → RCPT TO (per-recipient accept/reject) →
/// DATA → dot-stuffed payload → terminator. Returns `{accepted, rejected}` where
/// `rejected` is `[{address, code, message}]`. A MAIL-FROM 5xx (or no accepted
/// recipient) is a Tier-1 error string carrying the server text.
async fn smtp_transact(state: &mut SmtpClientState, env: &Envelope) -> Result<Value, String> {
    // MAIL FROM.
    let mail = state.command(&format!("MAIL FROM:<{}>", env.from)).await?;
    if !mail.is_positive() {
        return Err(format!("MAIL FROM rejected: {} {}", mail.code, mail.text));
    }

    // RCPT TO per recipient; collect accepts + rejects.
    let mut accepted: Vec<Value> = Vec::new();
    let mut rejected: Vec<Value> = Vec::new();
    for rcpt in &env.recipients {
        let reply = state.command(&format!("RCPT TO:<{}>", rcpt)).await?;
        if reply.is_positive() {
            accepted.push(Value::str(rcpt.clone()));
        } else {
            let mut o = indexmap::IndexMap::new();
            o.insert("address".to_string(), Value::str(rcpt.clone()));
            o.insert("code".to_string(), Value::int(reply.code as i64));
            o.insert("message".to_string(), Value::str(reply.text.clone()));
            rejected.push(Value::object(o));
        }
    }
    if accepted.is_empty() {
        return Err("all recipients were rejected by the server".to_string());
    }

    // DATA.
    let data = state.command("DATA").await?;
    if data.code != 354 {
        return Err(format!("DATA rejected: {} {}", data.code, data.text));
    }
    let payload = dot_stuff(&env.raw);
    state.write_raw(&payload).await?;
    let queued = state.read_reply().await?;
    if !queued.is_positive() {
        return Err(format!("message rejected after DATA: {} {}", queued.code, queued.text));
    }

    let mut result = indexmap::IndexMap::new();
    result.insert("accepted".to_string(), Value::array(accepted));
    result.insert("rejected".to_string(), Value::array(rejected));
    Ok(Value::object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_stuffing_leading_dots() {
        let raw = ".leading\n..two\nnormal";
        let out = String::from_utf8(dot_stuff(raw)).unwrap();
        assert!(out.contains("..leading\r\n"), "{out:?}");
        assert!(out.contains("...two\r\n"), "{out:?}");
        assert!(out.contains("normal\r\n"), "{out:?}");
        assert!(out.ends_with(".\r\n"), "missing terminator: {out:?}");
    }

    #[test]
    fn default_ports() {
        assert_eq!(default_port(TlsMode::Starttls), 587);
        assert_eq!(default_port(TlsMode::Implicit), 465);
        assert_eq!(default_port(TlsMode::None), 25);
    }

    #[test]
    fn ehlo_domain_uses_localhost_for_ip() {
        assert_eq!(ehlo_domain("127.0.0.1"), "localhost");
        assert_eq!(ehlo_domain("mail.example.com"), "mail.example.com");
    }
}
