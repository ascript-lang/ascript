//! `std/io` — standard-input reading (feature `sys`), spec §P2c.
//!
//! Provides three async functions over process stdin:
//!
//! - `io.readLine() -> string | nil`  — one line without trailing `\n`; `nil` at EOF.
//! - `io.readAll() -> string`         — all remaining stdin as UTF-8 lossy.
//! - `io.readLines() -> array<string>` — all lines (each without `\n`).
//!
//! **Buffering:** multiple `readLine` calls must share a single `BufReader` so
//! that bytes read ahead during one call are not silently dropped before the next.
//! We store the `BufReader<Stdin>` in the interpreter's resource table under the
//! fixed id `STDIN_RESOURCE_ID` (created lazily on first access). The take-out-
//! across-await / return pattern (from CLAUDE.md) ensures no `RefCell` borrow is
//! held across any `.await`.

use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState, STDIN_RESOURCE_ID};
use crate::span::Span;
use crate::value::Value;
use std::rc::Rc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("readLine", super::bi("io.readLine")),
        ("readAll", super::bi("io.readAll")),
        ("readLines", super::bi("io.readLines")),
    ]
}

impl Interp {
    /// Module-level dispatch for `std/io`.
    pub(crate) async fn call_io(
        &self,
        func: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "readLine" => self.io_read_line(span).await,
            "readAll" => self.io_read_all(span).await,
            "readLines" => self.io_read_lines(span).await,
            _ => Err(AsError::at(format!("std/io has no function '{}'", func), span).into()),
        }
    }

    /// Lazily acquire (or create) the shared stdin BufReader.
    /// Returns it taken out of the resource table — the caller MUST put it back
    /// (or decide not to, on EOF/close) using `return_resource`.
    fn take_stdin_reader(&self) -> Box<BufReader<tokio::io::Stdin>> {
        match self.take_resource(STDIN_RESOURCE_ID) {
            Some(ResourceState::StdinReader(r)) => r,
            // First call: create the reader and return it directly (never stored yet).
            _ => Box::new(BufReader::new(tokio::io::stdin())),
        }
    }

    async fn io_read_line(&self, span: Span) -> Result<Value, Control> {
        // Take the reader OUT before awaiting — no RefCell borrow across .await.
        let mut reader = self.take_stdin_reader();
        let mut buf = Vec::new();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => {
                // EOF: store the (exhausted) reader back so a subsequent call finds
                // it and returns EOF again immediately instead of recreating it.
                self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                Ok(Value::Nil)
            }
            Ok(_) => {
                // Strip trailing \r\n or \n.
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                }
                self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                Ok(Value::Str(
                    String::from_utf8_lossy(&buf).into_owned().into(),
                ))
            }
            Err(e) => {
                // Put the reader back so a transient error doesn't discard any
                // kernel-buffered bytes (consistent with readAll/readLines).
                self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                Err(AsError::at(format!("io.readLine failed: {}", e), span).into())
            }
        }
    }

    async fn io_read_all(&self, span: Span) -> Result<Value, Control> {
        let mut reader = self.take_stdin_reader();
        // Read raw bytes then decode LOSSY (spec): invalid UTF-8 becomes the
        // replacement char rather than erroring (read_to_string would Err here).
        let mut buf = Vec::new();
        match reader.read_to_end(&mut buf).await {
            Ok(_) => {
                // Return the (exhausted) reader so a subsequent call doesn't panic.
                self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                Ok(Value::Str(
                    String::from_utf8_lossy(&buf).into_owned().into(),
                ))
            }
            Err(e) => {
                // On error we still put the reader back (state unknown, but keeping
                // it means a retry is possible; callers should treat this as fatal).
                self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                Err(AsError::at(format!("io.readAll failed: {}", e), span).into())
            }
        }
    }

    async fn io_read_lines(&self, span: Span) -> Result<Value, Control> {
        let mut reader = self.take_stdin_reader();
        let mut lines: Vec<Value> = Vec::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if buf.last() == Some(&b'\n') {
                        buf.pop();
                        if buf.last() == Some(&b'\r') {
                            buf.pop();
                        }
                    }
                    lines.push(Value::Str(
                        String::from_utf8_lossy(&buf).into_owned().into(),
                    ));
                }
                Err(e) => {
                    self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
                    return Err(
                        AsError::at(format!("io.readLines failed: {}", e), span).into()
                    );
                }
            }
        }
        self.return_resource(STDIN_RESOURCE_ID, ResourceState::StdinReader(reader));
        Ok(Value::Array(Rc::new(std::cell::RefCell::new(lines))))
    }
}
