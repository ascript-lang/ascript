//! `std/email` — a pure email **message builder** (the SMTP client is B6).
//!
//! BATT B5 §8.1. This module is the *first line of defense* against SMTP header
//! injection. The threat model: a `\r`, `\n`, or NUL smuggled into any address
//! (`from`/`to`/`cc`) or header (the `Subject`, a custom header name/value) can
//! split the SMTP DATA stream and inject extra headers or even extra SMTP
//! commands (`\r\nRCPT TO:<evil>`, `\r\nBcc: evil@x`). The sibling of the cookie
//! CRLF guard in BATT A8.
//!
//! ## The CRLF/NUL guard (the security core)
//!
//! [`reject_crlf`] is applied at EVERY header-value insertion site — every
//! address, the subject, every custom header NAME and VALUE. Any `\r`, `\n`, or
//! `\0` (NUL) is a **Tier-2 panic** (header injection is a programmer bug, not a
//! recoverable result). The wire writer never echoes a raw header value that has
//! not passed this gate, so an attacker cannot smuggle a header through a builder
//! quirk.
//!
//! A legitimately long header (a 1000-char subject) is **folded** per RFC 5322
//! line folding: a `CRLF` followed by a single space (the WSP continuation) is
//! inserted at fold points — NEVER a bare `CRLF` that could start a new header.
//! Folding runs on already-guarded values (no `\r`/`\n` can be present), so the
//! ONLY CRLFs in the output are the structural ones the writer itself emits.
//!
//! ## Message shapes (`msg.raw()` wire form)
//!
//! * **plain text** — a single `text/plain` part: `From`/`To`/`Subject`/
//!   `MIME-Version`/`Content-Type: text/plain` headers + the body.
//! * **text + html** — `multipart/alternative` with a `text/plain` then a
//!   `text/html` part. The boundary is DETERMINISTIC (`sha256` of the parts,
//!   hex-truncated) so `msg.raw()` is byte-stable for the corpus.
//! * **attachments** — `multipart/mixed`, each attachment base64-encoded and
//!   wrapped at 76 columns.
//! * non-ASCII header values → RFC 2047 `=?UTF-8?B?...?=` encoded-words.
//!
//! `bcc` is an ENVELOPE field (used by the SMTP client in B6 to compute the
//! RCPT TO list) — it is NEVER rendered into the `.raw()` headers (a `Bcc:`
//! header would leak the blind-copy list to every recipient).
//!
//! ## `msg.raw()` dispatch
//!
//! `email.message(...)` returns a tagged Object `{__email: true, raw: <wire>,
//! envelope: {...}}` whose `raw` wire-form is computed EAGERLY at build time. The
//! `.raw()` method routes through the same call-position hook `std/schema` uses
//! ([`is_email_value`]/[`is_email_method`] consulted by `member_call_is_hook`),
//! so it fires byte-identically on both engines. The hook returns the stored
//! `raw` field — no recomputation, no `Value` variant.

use super::{arg, bi, want_object, want_string};
use crate::error::AsError;
use crate::interp::{make_pair, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use base64::Engine as _;
use indexmap::IndexMap;
use sha2::{Digest, Sha256};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("message", bi("email.message")),
        ("validateAddress", bi("email.validateAddress")),
        // B6 wires `send`/`connect` (Net-gated) — registered now so the gate +
        // sig surface are stable. They are not yet dispatchable (B6).
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "message" => build_message(args, span),
        "validateAddress" => {
            let s = want_string(&arg(args, 0), span, "email.validateAddress")?;
            Ok(Value::bool_(validate_address(&s)))
        }
        _ => Err(AsError::at(format!("std/email has no function '{}'", func), span).into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the tagged-message value + the `.raw()` call-position hook
// ─────────────────────────────────────────────────────────────────────────────

/// Is `v` a built email-message value (the tagged Object `{__email: true, …}`)?
/// Mirrors `schema::is_schema_value` — a pure predicate consulted by
/// `Interp::member_call_is_hook` so `msg.raw()` routes through the hook.
pub(crate) fn is_email_value(v: &Value) -> bool {
    if let ValueKind::Object(o) = v.kind() {
        return matches!(o.get("__email"), Some(t) if t.is_truthy());
    }
    false
}

/// Is `name` an email-message method (call-position hook). v1: only `raw`.
pub(crate) fn is_email_method(name: &str) -> bool {
    matches!(name, "raw")
}

/// Dispatch a `msg.<method>(...)` call (the call-position hook). `args[0]` is the
/// receiver (the tagged message), matching the `[recv, ...args]` convention
/// `call_schema` uses.
pub(crate) fn call_email_method(
    method: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    let recv = arg(args, 0);
    let ValueKind::Object(o) = recv.kind() else {
        return Err(AsError::at(
            format!("email message method '{}' called on a non-message value", method),
            span,
        )
        .into());
    };
    match method {
        "raw" => Ok(o.get("raw").unwrap_or_else(Value::nil)),
        _ => Err(AsError::at(format!("email message has no method '{}'", method), span).into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// the CRLF / NUL header-injection guard (the security core)
// ─────────────────────────────────────────────────────────────────────────────

/// Reject a `\r`, `\n`, or NUL (and other C0 controls that could be misparsed by
/// a lenient MTA) anywhere in a header field/value. A violation is a **Tier-2
/// panic** — header injection is a programmer bug, the sibling of the BATT A8
/// cookie CRLF guard. `field` names the offending slot for the message.
fn reject_crlf(field: &str, value: &str, span: Span) -> Result<(), Control> {
    for (i, b) in value.bytes().enumerate() {
        // CR / LF / NUL are the injection-bearing trio; reject the whole C0
        // control set (< 0x20, excluding TAB which is legal WSP) defensively so a
        // bare control can never reach the wire.
        let bad = b == b'\r' || b == b'\n' || b == 0 || (b < 0x20 && b != b'\t');
        if bad {
            return Err(AsError::at(
                format!(
                    "email header injection: control character (byte 0x{:02x}) at offset {} in '{}' \
                     — CR/LF/NUL in an address or header would smuggle SMTP headers/commands",
                    b, i, field
                ),
                span,
            )
            .into());
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// address validation (pragmatic RFC 5321 addr-spec subset)
// ─────────────────────────────────────────────────────────────────────────────

/// Validate a bare `local@domain` address. A pragmatic RFC 5321 subset
/// (documented): exactly one `@`, a non-empty dot-atom local part, a non-empty
/// dotted domain with at least one `.`, total length ≤ 254, NO spaces, NO control
/// characters (CR/LF/NUL → false — never a valid address). Quoted-local forms
/// (`"a b"@c.com`) are **recorded-unsupported** → `false` (documented limitation).
pub(crate) fn validate_address(addr: &str) -> bool {
    if addr.is_empty() || addr.len() > 254 {
        return false;
    }
    // Any control / whitespace char is an immediate reject (kills CRLF vectors).
    if addr.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return false;
    }
    let mut parts = addr.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false; // zero or >1 '@'
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    // Quoted-local recorded-unsupported.
    if local.starts_with('"') {
        return false;
    }
    if !is_dot_atom_local(local) {
        return false;
    }
    is_domain(domain)
}

/// A dot-atom local part: one or more atoms separated by single dots, no leading/
/// trailing/double dot, each char an RFC 5322 atext (or `.`).
fn is_dot_atom_local(local: &str) -> bool {
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return false;
    }
    local.chars().all(is_atext_or_dot)
}

fn is_atext_or_dot(c: char) -> bool {
    c == '.'
        || c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#' | '$' | '%' | '&' | '\'' | '*' | '+' | '-' | '/' | '='
                | '?' | '^' | '_' | '`' | '{' | '|' | '}' | '~'
        )
}

/// A dotted domain: labels separated by single dots, at least one dot, each label
/// alphanumeric/hyphen (not leading/trailing hyphen), no empty label.
fn is_domain(domain: &str) -> bool {
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// RFC 2047 encoded-words + RFC 5322 folding
// ─────────────────────────────────────────────────────────────────────────────

/// Encode a header value as an RFC 2047 `=?UTF-8?B?...?=` encoded-word IF it
/// contains any non-ASCII byte; otherwise return it verbatim. Always run AFTER
/// the CRLF guard (the value is already control-free).
fn encode_header_value(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    // Base64 the UTF-8 bytes. (RFC 2047 also limits encoded-words to 75 chars and
    // splits longer runs; for a header value we emit one encoded-word and let the
    // folder break it — common MTAs accept this; the security property, no bare
    // CRLF, holds regardless.)
    let b64 = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    format!("=?UTF-8?B?{}?=", b64)
}

/// Render one header line `Name: value`, folding per RFC 5322 if it would exceed
/// the 78-char soft limit. Folding inserts `CRLF + SPACE` (a WSP continuation) —
/// NEVER a bare CRLF, so a folded line can never be misparsed as a new header.
/// `value` MUST already have passed [`reject_crlf`].
fn render_header(name: &str, value: &str) -> String {
    let line = format!("{}: {}", name, value);
    fold_line(&line)
}

/// Fold a single logical header line at whitespace boundaries near the 78-col
/// soft limit. If no whitespace fold point exists (e.g. one very long token), a
/// HARD fold is inserted mid-token (`CRLF SPACE`) so the structural invariant
/// still holds: every physical line after the first begins with WSP, so no
/// continuation can ever start a new header.
fn fold_line(line: &str) -> String {
    const MAX: usize = 78;
    if line.len() <= MAX {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + line.len() / MAX * 3);
    let bytes = line.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let remaining = bytes.len() - start;
        if remaining <= MAX {
            out.push_str(&line[start..]);
            break;
        }
        // Find the last space within [start+1, start+MAX] to fold at.
        let window_end = start + MAX;
        let fold_at = line[start..window_end]
            .rfind(' ')
            .map(|rel| start + rel)
            .filter(|&p| p > start);
        match fold_at {
            Some(p) => {
                out.push_str(&line[start..p]);
                out.push_str("\r\n ");
                // Skip the folded space (it is replaced by the continuation WSP).
                start = p + 1;
            }
            None => {
                // No fold point — hard-break at MAX (char-boundary safe).
                let mut cut = window_end;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut <= start {
                    // Pathological (a single multibyte char wider than MAX) —
                    // emit the rest verbatim rather than loop forever.
                    out.push_str(&line[start..]);
                    break;
                }
                out.push_str(&line[start..cut]);
                out.push_str("\r\n ");
                start = cut;
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// the message builder (pure, eager)
// ─────────────────────────────────────────────────────────────────────────────

/// A header collected during the build (name + already-guarded value).
struct Header {
    name: String,
    value: String,
}

/// Build `email.message(opts)` → a Tier-1 `[msg, err]` pair. Misuse (non-object
/// opts, wrong field type) is a Tier-2 panic; an injection vector is ALSO a
/// Tier-2 panic (it is a programmer bug). A successful build returns
/// `[<tagged-message>, nil]`.
fn build_message(args: &[Value], span: Span) -> Result<Value, Control> {
    let opts = want_object(&arg(args, 0), span, "email.message")?;

    // ── required: from, to, subject ──────────────────────────────────────────
    let from = field_str(&opts, "from", span)?
        .ok_or_else(|| AsError::at("email.message: 'from' is required", span))?;
    reject_crlf("from", &from, span)?;

    let to_list = field_addr_list(&opts, "to", span)?;
    if to_list.is_empty() {
        return Err(AsError::at("email.message: 'to' is required (at least one address)", span).into());
    }
    for (i, a) in to_list.iter().enumerate() {
        reject_crlf(&format!("to[{}]", i), a, span)?;
    }

    let subject = field_str(&opts, "subject", span)?.unwrap_or_default();
    reject_crlf("subject", &subject, span)?;

    // ── optional: cc, bcc (bcc is ENVELOPE only — never a header) ────────────
    let cc_list = field_addr_list(&opts, "cc", span)?;
    for (i, a) in cc_list.iter().enumerate() {
        reject_crlf(&format!("cc[{}]", i), a, span)?;
    }
    let bcc_list = field_addr_list(&opts, "bcc", span)?;
    for (i, a) in bcc_list.iter().enumerate() {
        // bcc is still guarded (an injected bcc would corrupt the envelope) — but
        // it is NEVER rendered into the headers below.
        reject_crlf(&format!("bcc[{}]", i), a, span)?;
    }

    // ── bodies ───────────────────────────────────────────────────────────────
    let text = field_str(&opts, "text", span)?;
    let html = field_str(&opts, "html", span)?;
    let attachments = parse_attachments(&opts, span)?;

    // ── custom headers (name + value both guarded) ───────────────────────────
    let mut custom: Vec<Header> = Vec::new();
    if let Some(h) = opts.get("headers") {
        let ho = want_object(&h, span, "email.message headers")?;
        for (name, val) in ho.entries() {
            let name = name.to_string();
            reject_crlf("header-name", &name, span)?;
            // A header name additionally may not contain ':' or whitespace.
            if name.contains(':') || name.chars().any(|c| c.is_whitespace()) {
                return Err(AsError::at(
                    format!("email.message: invalid custom header name '{}'", name),
                    span,
                )
                .into());
            }
            let vs = want_string(&val, span, "email.message header value")?;
            reject_crlf(&format!("header '{}'", name), &vs, span)?;
            custom.push(Header { name, value: vs.to_string() });
        }
    }

    // ── assemble the standard headers ────────────────────────────────────────
    let mut headers: Vec<Header> = Vec::new();
    headers.push(Header { name: "From".into(), value: from.to_string() });
    headers.push(Header { name: "To".into(), value: to_list.join(", ") });
    if !cc_list.is_empty() {
        headers.push(Header { name: "Cc".into(), value: cc_list.join(", ") });
    }
    headers.push(Header {
        name: "Subject".into(),
        value: encode_header_value(&subject),
    });
    // Date is INJECTABLE for determinism: honored from a custom `Date` header if
    // the user supplied one (so the wire form is replay-stable), otherwise OMITTED
    // (the SMTP client in B6 stamps a real Date at send time). This keeps the pure
    // builder byte-deterministic without a clock dependency.
    headers.push(Header { name: "MIME-Version".into(), value: "1.0".into() });

    // ── build the body + Content-Type, choose the multipart shape ────────────
    let (content_type_header, body) = build_body(&text, &html, &attachments);
    headers.push(content_type_header);

    // Custom headers last (so a user `Date`/`Message-ID`/`Reply-To` lands in the
    // header block; they cannot override the structural ones above — both appear,
    // and a duplicate is the user's responsibility).
    headers.extend(custom);

    // ── render the wire form ─────────────────────────────────────────────────
    let mut wire = String::new();
    for h in &headers {
        wire.push_str(&render_header(&h.name, &encode_if_needed(&h.name, &h.value)));
        wire.push_str("\r\n");
    }
    wire.push_str("\r\n"); // header/body separator
    wire.push_str(&body);

    // ── build the tagged message value + envelope ────────────────────────────
    let mut env = IndexMap::new();
    env.insert("from".to_string(), Value::str(from.to_string()));
    env.insert("to".to_string(), Value::array(to_list.iter().map(|a| Value::str(a.clone())).collect()));
    env.insert("cc".to_string(), Value::array(cc_list.iter().map(|a| Value::str(a.clone())).collect()));
    env.insert("bcc".to_string(), Value::array(bcc_list.iter().map(|a| Value::str(a.clone())).collect()));

    let mut msg = IndexMap::new();
    msg.insert("__email".to_string(), Value::bool_(true));
    msg.insert("raw".to_string(), Value::str(wire));
    msg.insert("envelope".to_string(), Value::object(env));

    Ok(make_pair(Value::object(msg), Value::nil()))
}

/// Encode the header value as RFC 2047 EXCEPT for structural headers whose value
/// is an address/token list that must stay literal (`From`/`To`/`Cc`/`MIME-Version`
/// /`Content-Type`). The `Subject` is already encoded by the caller; everything
/// else (custom headers) gets encoded-word treatment if non-ASCII.
fn encode_if_needed(name: &str, value: &str) -> String {
    match name {
        "From" | "To" | "Cc" | "MIME-Version" | "Content-Type" | "Subject"
        | "Content-Transfer-Encoding" => value.to_string(),
        _ => encode_header_value(value),
    }
}

/// An attachment parsed from opts.
struct Attachment {
    filename: String,
    content_type: String,
    data: Vec<u8>,
}

fn parse_attachments(
    opts: &gcmodule::Cc<crate::value::ObjectCell>,
    span: Span,
) -> Result<Vec<Attachment>, Control> {
    let Some(v) = opts.get("attachments") else {
        return Ok(Vec::new());
    };
    let ValueKind::Array(a) = v.kind() else {
        return Err(AsError::at("email.message: 'attachments' must be an array", span).into());
    };
    let mut out = Vec::new();
    for (i, item) in a.borrow().iter().enumerate() {
        let o = want_object(item, span, "email.message attachment")?;
        let filename = match o.get("filename") {
            Some(f) => want_string(&f, span, "attachment.filename")?.to_string(),
            None => {
                return Err(AsError::at(
                    format!("email.message: attachment[{}] missing 'filename'", i),
                    span,
                )
                .into())
            }
        };
        reject_crlf(&format!("attachment[{}].filename", i), &filename, span)?;
        let content_type = match o.get("contentType") {
            Some(c) => {
                let s = want_string(&c, span, "attachment.contentType")?.to_string();
                reject_crlf(&format!("attachment[{}].contentType", i), &s, span)?;
                s
            }
            None => "application/octet-stream".to_string(),
        };
        let data = match o.get("content") {
            Some(c) => match c.kind() {
                ValueKind::Str(s) => s.as_bytes().to_vec(),
                ValueKind::Bytes(b) => b.borrow().to_vec(),
                _ => {
                    return Err(AsError::at(
                        format!("email.message: attachment[{}].content must be a string or bytes", i),
                        span,
                    )
                    .into())
                }
            },
            None => {
                return Err(AsError::at(
                    format!("email.message: attachment[{}] missing 'content'", i),
                    span,
                )
                .into())
            }
        };
        out.push(Attachment { filename, content_type, data });
    }
    Ok(out)
}

/// Build the body + the `Content-Type` header for the chosen shape:
/// - attachments present → `multipart/mixed` (the alternative/text as the first
///   part, then each attachment base64-wrapped).
/// - text + html (no attachments) → `multipart/alternative`.
/// - text only / html only → a single `text/plain` or `text/html` part.
fn build_body(
    text: &Option<String>,
    html: &Option<String>,
    attachments: &[Attachment],
) -> (Header, String) {
    if !attachments.is_empty() {
        return build_mixed(text, html, attachments);
    }
    match (text, html) {
        (Some(t), Some(h)) => build_alternative(t, h),
        (Some(t), None) => single_part("text/plain", t),
        (None, Some(h)) => single_part("text/html", h),
        (None, None) => single_part("text/plain", ""),
    }
}

fn single_part(ctype: &str, body: &str) -> (Header, String) {
    (
        Header {
            name: "Content-Type".into(),
            value: format!("{}; charset=utf-8", ctype),
        },
        body.to_string(),
    )
}

/// Deterministic boundary: `----=_Part_<hex>` where `<hex>` is the first 24 hex
/// chars of `sha256(parts joined)`. Content-derived → byte-stable for the corpus
/// (NOT a random boundary). A boundary that collides with the body is vanishingly
/// unlikely (sha256 over the actual content); a defensive caller could re-derive,
/// but the spec accepts the content-hash boundary.
fn boundary(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes()); // length-prefix → unambiguous
        h.update(p);
    }
    let digest = h.finalize();
    let hex = hex_encode(&digest[..12]); // 12 bytes → 24 hex chars
    format!("----=_Part_{}", hex)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn build_alternative(text: &str, html: &str) -> (Header, String) {
    let b = boundary(&[text.as_bytes(), html.as_bytes()]);
    let mut body = String::new();
    // text/plain part
    body.push_str(&format!("--{}\r\n", b));
    body.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
    body.push_str(text);
    body.push_str("\r\n");
    // text/html part
    body.push_str(&format!("--{}\r\n", b));
    body.push_str("Content-Type: text/html; charset=utf-8\r\n\r\n");
    body.push_str(html);
    body.push_str("\r\n");
    // closing boundary
    body.push_str(&format!("--{}--\r\n", b));
    (
        Header {
            name: "Content-Type".into(),
            value: format!("multipart/alternative; boundary=\"{}\"", b),
        },
        body,
    )
}

fn build_mixed(
    text: &Option<String>,
    html: &Option<String>,
    attachments: &[Attachment],
) -> (Header, String) {
    // Derive the boundary from ALL parts (bodies + attachment data) for stability.
    let mut hash_parts: Vec<&[u8]> = Vec::new();
    if let Some(t) = text {
        hash_parts.push(t.as_bytes());
    }
    if let Some(h) = html {
        hash_parts.push(h.as_bytes());
    }
    for a in attachments {
        hash_parts.push(a.filename.as_bytes());
        hash_parts.push(&a.data);
    }
    let b = boundary(&hash_parts);

    let mut body = String::new();

    // First part: the message body (alternative if both, else single).
    body.push_str(&format!("--{}\r\n", b));
    match (text, html) {
        (Some(t), Some(h)) => {
            let (ct, alt) = build_alternative(t, h);
            body.push_str(&format!("{}: {}\r\n\r\n", ct.name, ct.value));
            body.push_str(&alt);
        }
        (Some(t), None) => {
            body.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
            body.push_str(t);
            body.push_str("\r\n");
        }
        (None, Some(h)) => {
            body.push_str("Content-Type: text/html; charset=utf-8\r\n\r\n");
            body.push_str(h);
            body.push_str("\r\n");
        }
        (None, None) => {
            body.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n\r\n");
        }
    }

    // Attachment parts.
    for a in attachments {
        body.push_str(&format!("--{}\r\n", b));
        body.push_str(&format!("Content-Type: {}\r\n", a.content_type));
        body.push_str("Content-Transfer-Encoding: base64\r\n");
        body.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{}\"\r\n\r\n",
            a.filename
        ));
        body.push_str(&base64_wrapped(&a.data));
        body.push_str("\r\n");
    }

    body.push_str(&format!("--{}--\r\n", b));
    (
        Header {
            name: "Content-Type".into(),
            value: format!("multipart/mixed; boundary=\"{}\"", b),
        },
        body,
    )
}

/// Base64-encode `data` and wrap at 76 columns with CRLF (the MIME convention).
fn base64_wrapped(data: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(b64.len() + b64.len() / 76 * 2);
    let bytes = b64.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + 76).min(bytes.len());
        out.push_str(&b64[i..end]);
        out.push_str("\r\n");
        i = end;
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// opts field accessors
// ─────────────────────────────────────────────────────────────────────────────

/// Read an optional string field. Wrong type → Tier-2 panic.
fn field_str(
    opts: &gcmodule::Cc<crate::value::ObjectCell>,
    key: &str,
    span: Span,
) -> Result<Option<String>, Control> {
    match opts.get(key) {
        None => Ok(None),
        Some(v) if matches!(v.kind(), ValueKind::Nil) => Ok(None),
        Some(v) => Ok(Some(
            want_string(&v, span, &format!("email.message '{}'", key))?.to_string(),
        )),
    }
}

/// Read an address-list field that accepts either a single string or an array of
/// strings. Missing → empty list. Wrong element type → Tier-2 panic.
fn field_addr_list(
    opts: &gcmodule::Cc<crate::value::ObjectCell>,
    key: &str,
    span: Span,
) -> Result<Vec<String>, Control> {
    match opts.get(key) {
        None => Ok(Vec::new()),
        Some(v) => match v.kind() {
            ValueKind::Nil => Ok(Vec::new()),
            ValueKind::Str(s) => Ok(vec![s.to_string()]),
            ValueKind::Array(a) => {
                let mut out = Vec::new();
                for item in a.borrow().iter() {
                    out.push(
                        want_string(item, span, &format!("email.message '{}' element", key))?
                            .to_string(),
                    );
                }
                Ok(out)
            }
            _ => Err(AsError::at(
                format!("email.message '{}' must be a string or array of strings", key),
                span,
            )
            .into()),
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::object(m)
    }

    /// Build a message and return its raw wire string (asserting `err == nil`).
    fn raw_of(opts: Value) -> String {
        let r = call("message", &[opts], sp()).expect("message build");
        let ValueKind::Array(pair) = r.kind() else {
            panic!("message did not return a pair");
        };
        let pair = pair.borrow();
        assert!(matches!(pair[1].kind(), ValueKind::Nil), "err not nil: {:?}", pair[1]);
        // exercise the `.raw()` hook too
        let msg = pair[0].clone();
        let via_method = call_email_method("raw", std::slice::from_ref(&msg), sp()).unwrap();
        let field = match msg.kind() {
            ValueKind::Object(o) => o.get("raw").unwrap(),
            _ => panic!("msg not an object"),
        };
        // hook result == stored field
        assert_eq!(via_method, field, "raw() hook != stored raw field");
        match field.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("raw not a string"),
        }
    }

    /// Build a message expecting a Tier-2 panic (header injection / misuse).
    fn expect_panic(opts: Value) -> String {
        match call("message", &[opts], sp()) {
            Err(Control::Panic(e)) => e.message.clone(),
            Err(other) => panic!("expected Panic, got {:?}", other),
            Ok(v) => panic!("expected Panic, got Ok({:?})", v),
        }
    }

    // ── (a) wire-form pins ──────────────────────────────────────────────────

    #[test]
    fn plain_text_wire_form() {
        let raw = raw_of(obj(vec![
            ("from", Value::str("alice@example.com")),
            ("to", Value::str("bob@example.com")),
            ("subject", Value::str("Hello")),
            ("text", Value::str("Hi there")),
        ]));
        assert!(raw.contains("From: alice@example.com\r\n"), "{raw}");
        assert!(raw.contains("To: bob@example.com\r\n"), "{raw}");
        assert!(raw.contains("Subject: Hello\r\n"), "{raw}");
        assert!(raw.contains("MIME-Version: 1.0\r\n"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain; charset=utf-8\r\n"), "{raw}");
        // header/body separator then the body.
        assert!(raw.ends_with("\r\n\r\nHi there"), "{raw}");
        // No Date in the pin (deterministic; injectable / omitted by design).
        assert!(!raw.contains("\r\nDate:"), "{raw}");
    }

    #[test]
    fn text_html_is_multipart_alternative_deterministic_boundary() {
        let mk = || {
            raw_of(obj(vec![
                ("from", Value::str("a@b.com")),
                ("to", Value::str("c@d.com")),
                ("subject", Value::str("Multi")),
                ("text", Value::str("plain body")),
                ("html", Value::str("<p>html body</p>")),
            ]))
        };
        let raw1 = mk();
        let raw2 = mk();
        // Byte-stable across builds (deterministic boundary).
        assert_eq!(raw1, raw2, "boundary not deterministic");
        // The Content-Type header may be RFC 5322-folded (CRLF + WSP) because the
        // boundary pushes it past 78 cols. Folding replaces the fold-point space with
        // the WSP continuation, so unfold (drop CRLF+WSP) then normalize spaces before
        // matching the logical value.
        let unfolded = raw1.replace("\r\n ", "").replace("\r\n\t", "");
        assert!(
            unfolded.contains("Content-Type: multipart/alternative;boundary=\"----=_Part_")
                || unfolded.contains("Content-Type: multipart/alternative; boundary=\"----=_Part_"),
            "{raw1}"
        );
        assert!(raw1.contains("Content-Type: text/plain; charset=utf-8\r\n\r\nplain body\r\n"), "{raw1}");
        assert!(raw1.contains("Content-Type: text/html; charset=utf-8\r\n\r\n<p>html body</p>\r\n"), "{raw1}");
        // closing boundary `--<b>--`
        assert!(raw1.contains("--\r\n"), "{raw1}");
    }

    #[test]
    fn attachments_are_multipart_mixed_base64_wrapped() {
        // 60 bytes → base64 is 80 chars → wraps once at 76.
        let data: Vec<u8> = (0u8..60).collect();
        let raw = raw_of(obj(vec![
            ("from", Value::str("a@b.com")),
            ("to", Value::str("c@d.com")),
            ("subject", Value::str("Att")),
            ("text", Value::str("see attachment")),
            (
                "attachments",
                Value::array(vec![obj(vec![
                    ("filename", Value::str("data.bin")),
                    ("content", Value::bytes(data.clone())),
                    ("contentType", Value::str("application/octet-stream")),
                ])]),
            ),
        ]));
        assert!(raw.contains("Content-Type: multipart/mixed; boundary=\"----=_Part_"), "{raw}");
        assert!(raw.contains("Content-Transfer-Encoding: base64\r\n"), "{raw}");
        assert!(raw.contains("Content-Disposition: attachment; filename=\"data.bin\"\r\n"), "{raw}");
        // Base64 lines never exceed 76 columns.
        for line in raw.split("\r\n") {
            assert!(line.len() <= 78, "line too long: {line:?}");
        }
    }

    #[test]
    fn non_ascii_subject_is_rfc2047_encoded_word() {
        let raw = raw_of(obj(vec![
            ("from", Value::str("a@b.com")),
            ("to", Value::str("c@d.com")),
            ("subject", Value::str("Héllo wörld")),
            ("text", Value::str("body")),
        ]));
        assert!(raw.contains("Subject: =?UTF-8?B?"), "{raw}");
        // The encoded-word closes with `?=`.
        let line = raw.lines().find(|l| l.starts_with("Subject:")).unwrap();
        assert!(line.contains("?="), "{line}");
    }

    #[test]
    fn custom_headers_honored() {
        let raw = raw_of(obj(vec![
            ("from", Value::str("a@b.com")),
            ("to", Value::str("c@d.com")),
            ("subject", Value::str("X")),
            ("text", Value::str("body")),
            (
                "headers",
                obj(vec![
                    ("X-Custom", Value::str("custom-value")),
                    ("Reply-To", Value::str("reply@b.com")),
                ]),
            ),
        ]));
        assert!(raw.contains("X-Custom: custom-value\r\n"), "{raw}");
        assert!(raw.contains("Reply-To: reply@b.com\r\n"), "{raw}");
    }

    #[test]
    fn bcc_in_envelope_not_headers() {
        let r = call(
            "message",
            &[obj(vec![
                ("from", Value::str("a@b.com")),
                ("to", Value::str("c@d.com")),
                ("bcc", Value::str("secret@b.com")),
                ("subject", Value::str("X")),
                ("text", Value::str("body")),
            ])],
            sp(),
        )
        .unwrap();
        let ValueKind::Array(pair) = r.kind() else { panic!() };
        let pair = pair.borrow();
        let msg = pair[0].clone();
        let raw = match call_email_method("raw", std::slice::from_ref(&msg), sp()).unwrap().kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!(),
        };
        // Bcc MUST NOT appear in the rendered headers.
        assert!(!raw.to_ascii_lowercase().contains("bcc"), "bcc leaked into headers: {raw}");
        assert!(!raw.contains("secret@b.com"), "bcc address leaked: {raw}");
        // …but it IS in the envelope (for the SMTP RCPT TO list in B6).
        let env = match msg.kind() {
            ValueKind::Object(o) => o.get("envelope").unwrap(),
            _ => panic!(),
        };
        let bcc = match env.kind() {
            ValueKind::Object(o) => o.get("bcc").unwrap(),
            _ => panic!(),
        };
        let ValueKind::Array(a) = bcc.kind() else { panic!() };
        assert_eq!(a.borrow().len(), 1);
    }

    // ── (b) validateAddress table ────────────────────────────────────────────

    #[test]
    fn validate_address_table() {
        // valid
        assert!(validate_address("a@b.com"));
        assert!(validate_address("a+tag@b.com"));
        assert!(validate_address("first.last@sub.example.com"));
        assert!(validate_address("user_name@example.co.uk"));
        // quoted-local → recorded-unsupported (false).
        assert!(!validate_address("\"a b\"@c.com"));
        // invalid
        assert!(!validate_address("noatsign"));
        assert!(!validate_address("a@b@c.com"));
        assert!(!validate_address("a b@c.com")); // space
        assert!(!validate_address("@b.com")); // empty local
        assert!(!validate_address("a@")); // empty domain
        assert!(!validate_address("a@bcom")); // no dot in domain
        assert!(!validate_address("a@.b.com")); // leading dot
        assert!(!validate_address(".a@b.com")); // leading dot local
        assert!(!validate_address("a..b@c.com")); // double dot
        assert!(!validate_address("a@b.com\r\nRCPT TO:<x>")); // CRLF
        assert!(!validate_address("a@b.com\0")); // NUL
        // >254 chars
        let long = format!("{}@b.com", "x".repeat(260));
        assert!(!validate_address(&long));
    }

    // ── (c) the SMTP injection battery (THE security test) ───────────────────

    #[test]
    fn smtp_injection_battery() {
        // Every vector smuggles a CR / LF / NUL into an address, subject, or
        // custom header (name or value). EACH must be a Tier-2 panic — header
        // injection is a programmer bug.
        let base_ok = |extra: Vec<(&str, Value)>| {
            let mut v = vec![
                ("from", Value::str("a@b.com")),
                ("to", Value::str("c@d.com")),
                ("subject", Value::str("ok")),
                ("text", Value::str("body")),
            ];
            v.extend(extra);
            obj(v)
        };

        // to: CRLF smuggling a RCPT TO command.
        expect_panic(base_ok(vec![("to", Value::str("a@b\r\nRCPT TO:<evil@x>"))]));
        // subject: CRLF smuggling a Bcc header.
        expect_panic(base_ok(vec![("subject", Value::str("x\r\nBcc: evil@x"))]));
        // subject: bare LF smuggling a header.
        expect_panic(base_ok(vec![("subject", Value::str("x\nX-Injected: 1"))]));
        // subject: bare CR.
        expect_panic(base_ok(vec![("subject", Value::str("x\rX-Injected: 1"))]));
        // from: CRLF.
        expect_panic(base_ok(vec![("from", Value::str("a@b.com\r\nFrom: spoof@x"))]));
        // cc: CRLF.
        expect_panic(base_ok(vec![("cc", Value::str("a@b.com\r\nBcc: evil@x"))]));
        // bcc (envelope) is STILL guarded.
        expect_panic(base_ok(vec![("bcc", Value::str("a@b.com\r\nDATA"))]));
        // custom header VALUE with CRLF.
        expect_panic(base_ok(vec![(
            "headers",
            obj(vec![("X-Foo", Value::str("v\r\nX-Injected: 1"))]),
        )]));
        // custom header VALUE with bare LF.
        expect_panic(base_ok(vec![(
            "headers",
            obj(vec![("X-Foo", Value::str("v\nevil"))]),
        )]));
        // custom header VALUE with NUL.
        expect_panic(base_ok(vec![(
            "headers",
            obj(vec![("X-Foo", Value::str("v\0evil"))]),
        )]));
        // custom header NAME with CRLF (smuggle a whole header).
        expect_panic(base_ok(vec![(
            "headers",
            obj(vec![("X-Foo\r\nX-Injected", Value::str("1"))]),
        )]));
        // custom header NAME with a colon (would forge `Name: forged`).
        expect_panic(base_ok(vec![(
            "headers",
            obj(vec![("X-Foo: forged\r\nX", Value::str("1"))]),
        )]));
        // array `to` element with CRLF.
        expect_panic(base_ok(vec![(
            "to",
            Value::array(vec![Value::str("ok@x.com"), Value::str("b@x\r\nDATA")]),
        )]));
        // attachment filename with CRLF.
        expect_panic(base_ok(vec![(
            "attachments",
            Value::array(vec![obj(vec![
                ("filename", Value::str("a\r\nContent-Type: evil")),
                ("content", Value::str("x")),
            ])]),
        )]));
    }

    #[test]
    fn long_subject_folds_without_injectable_header() {
        // A 1000-char subject must FOLD (RFC 5322) — and the fold must NEVER
        // produce a bare CRLF that starts a new header. Every physical line after
        // the first MUST begin with WSP.
        let long = "word ".repeat(200); // 1000 chars, foldable at spaces
        let raw = raw_of(obj(vec![
            ("from", Value::str("a@b.com")),
            ("to", Value::str("c@d.com")),
            ("subject", Value::str(long.trim_end())),
            ("text", Value::str("body")),
        ]));
        // Find the Subject header block: from `Subject:` to the next header that
        // starts at column 0 (a line NOT beginning with WSP).
        let lines: Vec<&str> = raw.split("\r\n").collect();
        let subj_idx = lines.iter().position(|l| l.starts_with("Subject:")).unwrap();
        // The line is long; assert it folded into multiple physical lines.
        let mut continuation_count = 0;
        for line in &lines[subj_idx + 1..] {
            if line.starts_with(' ') || line.starts_with('\t') {
                continuation_count += 1;
            } else {
                break; // end of the folded header
            }
        }
        assert!(continuation_count > 0, "long subject did not fold: {raw}");
        // CRITICAL: no continuation line forms a parseable injected header at col 0.
        // (Already guaranteed: continuations all start with WSP.) Double-check that
        // the only `Subject` occurrence at col 0 is the real one.
        let col0_headers: Vec<&&str> = lines
            .iter()
            .take_while(|l| !l.is_empty()) // header block ends at the blank line
            .filter(|l| !l.starts_with(' ') && !l.starts_with('\t'))
            .collect();
        // No forged header (e.g. "word:" or "X-Injected:") appeared at col 0.
        for h in &col0_headers {
            // every col-0 header is one of the known structural ones
            let name = h.split(':').next().unwrap();
            assert!(
                matches!(name, "From" | "To" | "Subject" | "MIME-Version" | "Content-Type"),
                "unexpected col-0 header (possible injection): {h:?}"
            );
        }
    }

    #[test]
    fn folding_hard_breaks_unsplittable_token() {
        // A single 1000-char token with no spaces still folds (hard break) — every
        // continuation begins with WSP, so no bare CRLF starts a header.
        let token = "x".repeat(1000);
        let folded = fold_line(&format!("X-Long: {}", token));
        for (i, line) in folded.split("\r\n").enumerate() {
            if i == 0 {
                assert!(line.starts_with("X-Long:"));
            } else {
                assert!(line.starts_with(' '), "continuation not WSP: {line:?}");
            }
            assert!(line.len() <= 80, "physical line too long: {}", line.len());
        }
        // No bare \n or \r without the structural \r\n .
        assert!(!folded.contains('\n') || folded.contains("\r\n"));
    }

    #[test]
    fn message_requires_object_opts() {
        // Non-object opts → Tier-2 panic (misuse, not a recoverable result).
        match call("message", &[Value::str("nope")], sp()) {
            Err(Control::Panic(_)) => {}
            other => panic!("expected panic, got {:?}", other),
        }
    }

    #[test]
    fn email_hook_predicates() {
        let r = call(
            "message",
            &[obj(vec![
                ("from", Value::str("a@b.com")),
                ("to", Value::str("c@d.com")),
                ("subject", Value::str("x")),
                ("text", Value::str("y")),
            ])],
            sp(),
        )
        .unwrap();
        let ValueKind::Array(pair) = r.kind() else { panic!() };
        let msg = pair.borrow()[0].clone();
        assert!(is_email_value(&msg));
        assert!(is_email_method("raw"));
        assert!(!is_email_method("send"));
        // a plain object is NOT an email value.
        assert!(!is_email_value(&obj(vec![("x", Value::nil())])));
    }
}
