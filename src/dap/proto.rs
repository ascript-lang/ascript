//! Hand-rolled DAP wire message structs (the minimal v1 set), serde-derived. We do
//! NOT pull an external DAP-types crate — the surface here is small and stable.
//!
//! Framing is identical to LSP: `Content-Length: N\r\n\r\n{json}` (see
//! [`read_message`]/[`write_message`]). Every protocol message has a monotonically
//! increasing `seq`; a `response`/`event` carries `type`, and a `response` echoes the
//! request's `seq` in `request_seq` plus the `command`.

use serde::Deserialize;
use serde_json::Value as Json;
use std::io::{BufRead, Write};

/// A decoded incoming DAP request. Only the fields we read are typed; everything else
/// rides in `arguments` as raw JSON and is pulled out per-command in the handlers.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    #[serde(default)]
    pub arguments: Json,
}

/// Read ONE Content-Length-framed JSON message from `r`. Returns `Ok(None)` on a
/// clean EOF (the editor closed stdin) so the server loop can exit. Mirrors the LSP
/// framing exactly (header lines terminated by `\r\n`, blank line, then the body).
pub fn read_message<R: BufRead>(r: &mut R) -> std::io::Result<Option<Request>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            // EOF before any header → the stream closed cleanly.
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Blank line → end of headers.
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }
    let len = match content_length {
        Some(len) => len,
        // A header block with no Content-Length is malformed; treat as EOF-ish.
        None => return Ok(None),
    };
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let req: Request = serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(req))
}

/// Serialize `msg` and write it to `w` with the Content-Length frame, flushing.
pub fn write_message<W: Write>(w: &mut W, msg: &Json) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).expect("DAP message serializes");
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

/// Build a `response` message envelope for `request` with the given success/body.
/// `seq` is the server's own outgoing sequence number.
pub fn response(seq: i64, request_seq: i64, command: &str, success: bool, body: Json) -> Json {
    serde_json::json!({
        "seq": seq,
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command,
        "body": body,
    })
}

/// Build an `event` message envelope (`seq`, `type:"event"`, `event`, `body`).
pub fn event(seq: i64, name: &str, body: Json) -> Json {
    serde_json::json!({
        "seq": seq,
        "type": "event",
        "event": name,
        "body": body,
    })
}
