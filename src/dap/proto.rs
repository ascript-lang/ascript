//! Hand-rolled DAP wire message structs (the minimal v1 set), serde-derived. We do
//! NOT pull an external DAP-types crate — the surface here is small and stable.
//!
//! Framing is identical to LSP: `Content-Length: N\r\n\r\n{json}` (see
//! [`read_message`]/[`write_message`]). Every protocol message has a monotonically
//! increasing `seq`; a `response`/`event` carries `type`, and a `response` echoes the
//! request's `seq` in `request_seq` plus the `command`.

use serde::Deserialize;
use serde_json::Value as Json;
use std::io::{BufRead, Read, Write};

/// Upper bound on a single DAP message body, enforced BEFORE the body buffer is
/// allocated. DAP messages are tiny (breakpoints, stack frames, evaluate strings);
/// 64 MiB is enormously generous. An untrusted/malformed `Content-Length:` larger
/// than this (e.g. `4000000000` → a ~4 GB `vec![0u8; len]` → OOM abort) is rejected
/// as a malformed frame instead of being allocated (DoS hardening).
const MAX_DAP_MESSAGE: usize = 64 * 1024 * 1024;

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
    // Bound the header block too: a malicious client could otherwise stream an endless
    // single header line with no `\n` (unbounded `String` growth → OOM) or an unbounded
    // number of junk header lines. DAP/LSP headers are a handful of short lines; these
    // caps are enormously generous and never trip on a well-formed frame.
    const MAX_HEADER_LINE: u64 = 8 * 1024;
    const MAX_HEADER_LINES: usize = 64;
    for _ in 0..MAX_HEADER_LINES {
        let mut line = String::new();
        // `take` caps a single line read so an unterminated line cannot grow without bound.
        let n = r.by_ref().take(MAX_HEADER_LINE).read_line(&mut line)?;
        if n == 0 {
            // EOF before any header → the stream closed cleanly.
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Blank line → end of headers.
            break;
        }
        // A line that hit the byte cap without a newline is a malformed/oversize header.
        // Unifying policy (mirrors the body-cap site below): `Err` = an active
        // resource-abuse attempt (an oversize line the server loop logs); `Ok(None)` =
        // clean EOF / benign-malformed (a missing/unparseable header → nothing to abuse).
        if n as u64 == MAX_HEADER_LINE && !line.ends_with('\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "DAP header line exceeds the size cap",
            ));
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }
    let len = match content_length {
        Some(len) if len <= MAX_DAP_MESSAGE => len,
        // An oversize length would allocate `vec![0u8; len]` BEFORE reading any body —
        // a malicious/malformed `Content-Length` (e.g. 4 GB) is an OOM/DoS vector. Reject
        // it as a malformed frame: an `io::Error` so the server loop logs it (it treats
        // both `Ok(None)` and `Err` as a clean teardown, but `Err` surfaces the cause).
        Some(len) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Content-Length {len} exceeds the {MAX_DAP_MESSAGE}-byte cap"),
            ));
        }
        // A header block with no (or an unparseable/negative) Content-Length is
        // malformed; treat as EOF-ish.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A valid small framed message parses into a `Request`.
    #[test]
    fn parses_valid_message() {
        let body = br#"{"seq":1,"command":"initialize","arguments":{}}"#;
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(body);
        let mut cur = Cursor::new(frame);
        let req = read_message(&mut cur).expect("parse ok").expect("some");
        assert_eq!(req.seq, 1);
        assert_eq!(req.command, "initialize");
    }

    /// An oversize `Content-Length` is rejected as an `io::Error` WITHOUT allocating
    /// the (4 GB) body buffer. The header is well under the cap to read; the length
    /// value is the attack. The test completes instantly and does not OOM, proving the
    /// cap fires before `vec![0u8; len]`.
    #[test]
    fn oversize_content_length_rejected_cheaply() {
        // ~4 GB requested; nothing follows the header (no body bytes at all).
        let frame = b"Content-Length: 4000000000\r\n\r\n".to_vec();
        let mut cur = Cursor::new(frame);
        let err = read_message(&mut cur).expect_err("oversize must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A length just over the cap is rejected; a length AT the cap is accepted by the
    /// length check (then fails on the truncated body via `read_exact`, an `UnexpectedEof`
    /// — proving the boundary is `<=`, not the OOM path).
    #[test]
    fn cap_boundary() {
        let over = MAX_DAP_MESSAGE + 1;
        let mut cur = Cursor::new(format!("Content-Length: {over}\r\n\r\n").into_bytes());
        let err = read_message(&mut cur).expect_err("over-cap rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // At the cap the length check passes; with no body, `read_exact` hits EOF.
        // We DON'T allocate the full 64 MiB cheaply-testable, so use a tiny in-cap value
        // with a truncated body to confirm the non-oversize path still flows to read_exact.
        let small = b"Content-Length: 100\r\n\r\n".to_vec();
        let mut cur2 = Cursor::new(small);
        let err2 = read_message(&mut cur2).expect_err("truncated body");
        assert_eq!(err2.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// A non-numeric / negative `Content-Length` fails the `parse::<usize>()` → `None`
    /// → clean `Ok(None)` (no allocation, no panic).
    #[test]
    fn unparseable_content_length_is_clean_eof() {
        for bad in ["Content-Length: -1\r\n\r\n", "Content-Length: abc\r\n\r\n"] {
            let mut cur = Cursor::new(bad.as_bytes().to_vec());
            let out = read_message(&mut cur).expect("no error");
            assert!(out.is_none(), "{bad:?} should be Ok(None)");
        }
    }

    /// An endless header line with no newline (the sibling OOM vector to the body cap)
    /// is rejected as `InvalidData` once it hits the per-line byte cap — it does NOT grow
    /// the `String` without bound. We feed more than the cap of non-newline bytes.
    #[test]
    fn oversize_header_line_rejected() {
        let mut frame = b"X-Junk: ".to_vec();
        frame.extend(std::iter::repeat_n(b'a', 64 * 1024)); // > MAX_HEADER_LINE, no '\n'
        let mut cur = Cursor::new(frame);
        let err = read_message(&mut cur).expect_err("oversize header line");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// More than `MAX_HEADER_LINES` non-blank header lines with NO blank-line terminator:
    /// the loop bounds out after the cap WITHOUT finding a Content-Length, so it returns a
    /// clean `Ok(None)` — no panic, no body allocation (the constant that bounds the header
    /// loop is the thing under test here).
    #[test]
    fn too_many_header_lines_is_clean_eof() {
        let mut frame = String::new();
        for i in 0..70 {
            frame.push_str(&format!("X-Junk-{i}: value\r\n"));
        }
        // deliberately NO blank-line terminator
        let mut cur = Cursor::new(frame.into_bytes());
        assert!(read_message(&mut cur).expect("no error").is_none());
    }

    /// A malformed JSON body (valid frame, in-cap length, non-JSON content) is a clean
    /// `InvalidData` error, never a panic.
    #[test]
    fn malformed_json_body_is_clean_error() {
        let body = b"not json at all";
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(body);
        let mut cur = Cursor::new(frame);
        let err = read_message(&mut cur).expect_err("bad json");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
