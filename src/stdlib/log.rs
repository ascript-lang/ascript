//! `std/log` — leveled, structured logging. Records carry a level, a message,
//! and merged object fields; emitted to stderr (live) or a capture buffer (tests).
//! Serialization is total (never panics) via `json::to_json_lossy`.
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("debug", super::bi("log.debug")),
        ("info", super::bi("log.info")),
        ("warn", super::bi("log.warn")),
        ("error", super::bi("log.error")),
        ("setLevel", super::bi("log.setLevel")),
        ("setFormat", super::bi("log.setFormat")),
    ]
}
