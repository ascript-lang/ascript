//! `std/workflow` — durable execution via event-sourced deterministic replay
//! (SP9 §2). The Temporal/Restate/Cloudflare model, NOT continuation serialization
//! (architecturally impossible with live native handles — see spec §2.1).
//!
//! A **workflow** is deterministic AScript code: control flow plus calls to
//! **activities** through the workflow **ctx**. Non-deterministic effects (I/O, time,
//! randomness) happen ONLY inside activities; the engine persists an append-only
//! **event log** of every activity's *result* (a `Value` serialized via
//! `json::to_json_lossy`). On `resume` after a crash, the workflow code re-runs from
//! the top, but each `ctx.call`/`ctx.now`/`ctx.random` returns its recorded result
//! from the log instead of re-executing — so the workflow deterministically
//! fast-forwards to where it left off. The continuation is reconstructed by replay,
//! never serialized; workflow code runs on an ordinary stack (no model-2b VM).
//!
//! Built on the SP9 §3 [`crate::det::DeterminismContext`]: `workflow.run` enters
//! Record mode, `workflow.resume` enters Replay mode primed from the log.
//!
//! Surface (tagged Objects, no new `Value` variant — mirrors `std/schema`):
//! - `activity(name, fn)` → `{__kind:"activity", name, fn}`.
//! - the `ctx` passed to a workflow body → `{__kind:"workflow_ctx"}`; its methods
//!   (`call`/`now`/`random`/`uuid`/`sleep`) are intercepted at the call site (the
//!   same hook `std/schema` uses) and routed to [`crate::interp::Interp`].
//! - `run(wf, input, {log})` / `resume(wf, input, {log})` drive the workflow.
//!
//! Feature-gated on `workflow` (depends on `data` for the JSON log); under
//! `--no-default-features` the whole module is compiled out and `import
//! "std/workflow"` is an unknown-module error (symmetric on both engines).

use crate::det::{DetEvent, DeterminismContext, Mode};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::{OwnedKind, Value, ValueKind};
use std::cell::RefCell;
use std::rc::Rc;

/// The tag marking a workflow-context Object. The call-site hook in the evaluators
/// checks this to route `ctx.<method>(...)`.
pub const CTX_KIND: &str = "workflow_ctx";
/// The tag marking an activity Object.
pub const ACTIVITY_KIND: &str = "activity";

/// `std/workflow` exports brought in by `import`.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("activity", super::bi("workflow.activity")),
        ("run", super::bi("workflow.run")),
        ("resume", super::bi("workflow.resume")),
    ]
}

/// Build a tagged activity Object `{__kind:"activity", name, fn}`.
pub fn make_activity(name: String, func: Value) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("__kind".to_string(), Value::str(ACTIVITY_KIND));
    m.insert("name".to_string(), Value::str(name));
    m.insert("fn".to_string(), func);
    Value::object(m)
}

/// Build the tagged workflow-context Object passed to a workflow body. It carries no
/// per-instance state (the recorded event stream lives in the `Interp`'s
/// `DeterminismContext`); the tag is what the call-site hook dispatches on.
pub fn make_ctx() -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("__kind".to_string(), Value::str(CTX_KIND));
    Value::object(m)
}

/// True iff `v` is the workflow-context tagged Object.
pub fn is_ctx_value(v: &Value) -> bool {
    tagged_kind(v) == Some(CTX_KIND)
}

/// True iff `v` is an activity tagged Object.
pub fn is_activity_value(v: &Value) -> bool {
    tagged_kind(v) == Some(ACTIVITY_KIND)
}

/// The methods callable on a `ctx` (the call-site hook routes these to
/// `Interp::call_workflow_ctx`). A bare `ctx.now` (member read, no call) is NOT a
/// method — it reads the (absent) field, returning nil — mirroring schema's
/// call-position-only limitation.
pub fn is_ctx_method(name: &str) -> bool {
    matches!(name, "call" | "now" | "random" | "uuid" | "sleep")
}

/// Read the `__kind` tag of an Object value, if it is one.
fn tagged_kind(v: &Value) -> Option<&'static str> {
    if let ValueKind::Object(o) = v.kind() {
        if let Some(ValueKind::Str(k)) = o.get("__kind").as_ref().map(Value::kind) {
            return match k.as_ref() {
                CTX_KIND => Some(CTX_KIND),
                ACTIVITY_KIND => Some(ACTIVITY_KIND),
                _ => None,
            };
        }
    }
    None
}

/// Serialize a `Value` to a JSON string for the event log via the total
/// `to_json_lossy` codec (the same `std/log` relies on — cycles→`"[Circular]"`,
/// NaN→null, never panics).
pub fn to_json_string(v: &Value) -> String {
    crate::stdlib::json::to_json_lossy(v, &mut Vec::new()).to_string()
}

/// Parse a JSON string from the event log back into a `Value`.
pub fn from_json_string(s: &str) -> Value {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(jv) => crate::stdlib::json::to_ascript(&jv),
        Err(_) => Value::nil(),
    }
}

/// A stable hash of an activity's name + JSON-serialized args, used to pin the call
/// signature so a workflow-code change that reorders effects is caught as a
/// non-determinism error rather than silently replaying a wrong value.
pub fn signature_hash(name: &str, args: &[Value]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    for a in args {
        to_json_string(a).hash(&mut h);
    }
    h.finish()
}

/// Serialize the in-memory `DetEvent` stream to the newline-delimited JSON log
/// format (spec §2.3): one record per line, `seq`-ordered. Reuses `serde_json` (the
/// `data` feature the `workflow` feature depends on).
fn events_to_log(events: &[DetEvent]) -> String {
    let mut out = String::new();
    for (seq, ev) in events.iter().enumerate() {
        let rec = event_to_json(seq, ev);
        out.push_str(&rec.to_string());
        out.push('\n');
    }
    out
}

/// WARM C (§4.3/§4.4): serialize one `(seq, event)` into a crc-framed newline-JSON
/// record line for the GROUP appender. The crc is the hand-rolled [`crate::det::crc32`]
/// over the record's compact JSON bytes WITHOUT the crc field; the `"crc"` field is
/// then added LAST and the object re-serialized + newline-terminated. On open, the
/// repair recomputes the crc the same way (remove crc → re-serialize → compare), so a
/// torn final append (a partial `write`) fails the crc and is truncated away.
///
/// This is the `fn(usize, &DetEvent) -> Vec<u8>` handed to [`crate::det::GroupAppender`]
/// so `det.rs` itself stays serde-free (and builds under `--no-default-features`).
fn group_record_line(seq: usize, ev: &DetEvent) -> Vec<u8> {
    let mut obj = event_to_json(seq, ev);
    let bytes_sans_crc = obj.to_string();
    let crc = crate::det::crc32(bytes_sans_crc.as_bytes());
    if let Some(map) = obj.as_object_mut() {
        map.insert("crc".to_string(), serde_json::json!(crc));
    }
    let mut line = obj.to_string();
    line.push('\n');
    line.into_bytes()
}

/// WARM C (§4.4): the crc-framed terminal `WorkflowCompleted` line for the group path.
/// Carries a crc but NO `seq` (matching `completed_result`'s last-line check — a
/// completion record is identified by `kind`, never by sequence).
fn group_completion_line(result: &Value) -> Vec<u8> {
    let mut rec = serde_json::json!({
        "kind": "WorkflowCompleted",
        "result": serde_json::from_str::<serde_json::Value>(&to_json_string(result))
            .unwrap_or(serde_json::Value::Null),
    });
    let bytes_sans_crc = rec.to_string();
    let crc = crate::det::crc32(bytes_sans_crc.as_bytes());
    if let Some(map) = rec.as_object_mut() {
        map.insert("crc".to_string(), serde_json::json!(crc));
    }
    let mut line = rec.to_string();
    line.push('\n');
    line.into_bytes()
}

/// WARM C (§4.4): find the byte length of the VALID CONTIGUOUS PREFIX of a group log.
/// A line is part of the valid prefix iff it is (a) newline-terminated, (b) valid JSON,
/// (c) crc-carrying with a VERIFYING crc (a legacy crc-less line — e.g. a rename-written
/// log — is accepted), and (d) `seq`-contiguous with its predecessor (records carry a
/// monotone seq starting at 0; the `WorkflowCompleted` terminal carries none and ends
/// the scan as a valid terminator). The first line failing any check ends the prefix;
/// everything from there on is the torn/divergent tail to truncate.
fn valid_prefix_len(bytes: &[u8]) -> usize {
    let mut prefix_end = 0usize;
    let mut pos = 0usize;
    let mut expect_seq: i64 = 0;
    while pos < bytes.len() {
        // A line must be newline-terminated to be part of the durable prefix (a final
        // line with no '\n' is a torn partial append).
        let Some(rel_nl) = bytes[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let line_end = pos + rel_nl; // index of '\n'
        let line = &bytes[pos..line_end];
        let line_str = std::str::from_utf8(line).ok();
        let Some(line_str) = line_str else { break };
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            // A blank line is benign whitespace — include it and continue.
            pos = line_end + 1;
            prefix_end = pos;
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            break;
        };
        let Some(map) = v.as_object() else { break };
        // crc check (if present): recompute over the object sans crc.
        if let Some(crc_val) = map.get("crc") {
            let Some(carried) = crc_val.as_u64() else { break };
            let mut without = map.clone();
            without.remove("crc");
            let recomputed = crate::det::crc32(
                serde_json::Value::Object(without).to_string().as_bytes(),
            );
            if recomputed as u64 != carried {
                break;
            }
        }
        // seq-contiguity check (if present). A `WorkflowCompleted` carries no seq and is
        // a valid terminator — accept it and stop scanning (nothing legitimately follows).
        if let Some(seq) = map.get("seq").and_then(|s| s.as_i64()) {
            if seq != expect_seq {
                break;
            }
            expect_seq += 1;
        } else if map.get("kind").and_then(|k| k.as_str()) == Some("WorkflowCompleted") {
            // Terminal record: include it, then stop.
            pos = line_end + 1;
            prefix_end = pos;
            break;
        }
        // This line is part of the valid prefix.
        pos = line_end + 1;
        prefix_end = pos;
    }
    prefix_end
}

/// WARM C (§4.4): open the group log for resume, REPAIRING a torn tail by
/// prefix-truncation. Reads the file, computes the valid contiguous prefix
/// ([`valid_prefix_len`]), physically `set_len`s the file to that boundary (the
/// truncation only ever SHRINKS — never extends — and an `ftruncate` error surfaces),
/// and returns `(append_file, repaired_prefix_text)`. The returned file is opened in
/// append mode positioned at the prefix end. A non-existent log is treated as a fresh
/// run (create empty). Used only on the group resume path.
fn open_group_log(path: &str, span: Span) -> Result<(std::fs::File, String), Control> {
    use std::io::{Seek, SeekFrom};
    let existing = std::fs::read(path).unwrap_or_default();
    let prefix_len = valid_prefix_len(&existing);
    let prefix_text = String::from_utf8_lossy(&existing[..prefix_len]).into_owned();

    // Open (create if absent) read-write, truncate to the valid prefix, seek to end.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| {
            AsError::at(format!("workflow: cannot open group log '{}': {}", path, e), span)
        })?;
    // set_len only shrinks here (prefix_len <= existing.len()); a failure surfaces.
    f.set_len(prefix_len as u64)
        .map_err(|e| AsError::at(format!("workflow: log repair (truncate) failed: {}", e), span))?;
    f.seek(SeekFrom::End(0))
        .map_err(|e| AsError::at(format!("workflow: log seek failed: {}", e), span))?;
    Ok((f, prefix_text))
}

/// One `DetEvent` → a JSON log record.
fn event_to_json(seq: usize, ev: &DetEvent) -> serde_json::Value {
    use serde_json::json;
    match ev {
        DetEvent::ClockRead { value } => json!({"seq": seq, "kind": "ClockRead", "value": value}),
        DetEvent::MonotonicRead { value } => {
            json!({"seq": seq, "kind": "MonotonicRead", "value": value})
        }
        DetEvent::RandomRead { value } => json!({"seq": seq, "kind": "RandomRead", "value": value}),
        // Task 0.19c: a seeded byte draw (`uuid.v4`/`uuid.v7`/`crypto.randomBytes`/salts).
        // The drawn bytes are stored as a JSON number array (same convention as the
        // boundary byte events above — no new base64 dep; the `workflow` feature only
        // pulls `data`/`serde_json`).
        DetEvent::BytesRead { bytes } => json!({"seq": seq, "kind": "BytesRead", "bytes": bytes}),
        DetEvent::TimerSet { wake } => json!({"seq": seq, "kind": "TimerSet", "wake": wake}),
        DetEvent::ActivityCompleted {
            name,
            args_hash,
            result_json,
        } => json!({
            "seq": seq,
            "kind": "ActivityCompleted",
            "name": name,
            "argsHash": args_hash.to_string(),
            // The activity result is itself JSON; embed it as a parsed value so the
            // log is human-inspectable, falling back to the raw string.
            "result": serde_json::from_str::<serde_json::Value>(result_json)
                .unwrap_or(serde_json::Value::Null),
        }),
        // Workers Spec B (Task 12): cross-isolate boundary events. The structured-
        // clone bytes are stored as a JSON number array (no new base64 dep — the
        // `workflow` feature only pulls `data`/`serde_json`).
        DetEvent::ActorCall {
            method,
            result,
            panic,
        } => json!({
            "seq": seq,
            "kind": "ActorCall",
            "method": method,
            "result": result,
            "panic": panic,
        }),
        DetEvent::GeneratorYield { value, panic } => json!({
            "seq": seq,
            "kind": "GeneratorYield",
            "value": value,
            "panic": panic,
        }),
        // FFI Task 10 (§7A): a recorded foreign `sym.call`. `ret` is the marshalled
        // return (a tagged primitive); `outParams` snapshots every `Bytes` out-param's
        // post-call contents (`[index, byteArray]` pairs).
        DetEvent::FfiCall { ret, out_params } => {
            let (ret_kind, ret_value) = ffi_ret_to_json(ret);
            json!({
                "seq": seq,
                "kind": "FfiCall",
                "retKind": ret_kind,
                "retValue": ret_value,
                "outParams": out_params
                    .iter()
                    .map(|(i, b)| json!([i, b]))
                    .collect::<Vec<_>>(),
            })
        }
        // REPLAY §2.3/§2.5: `StdlibCall`/`NativeCall` are CLI-trace-only events (the
        // `ASTRC` binary trace, a separate REPLAY artifact). They are NEVER installed in
        // a `std/workflow` determinism context, so the workflow newline-JSON log codec
        // never encodes them in practice. A defensive marker keeps the match exhaustive
        // without inventing a JSON projection of the airlock bytes the workflow log can't
        // carry (`log_to_events` likewise never parses these kinds).
        DetEvent::StdlibCall { module, func, .. } => json!({
            "seq": seq, "kind": "StdlibCall", "module": module, "func": func,
        }),
        DetEvent::NativeCall { vid, method, .. } => json!({
            "seq": seq, "kind": "NativeCall", "vid": vid, "method": method,
        }),
    }
}

/// FFI Task 10: encode an [`FfiRet`] as `(kind, value)` for the JSON log. The value
/// is always a JSON number (an int as i64, a float as f64, void as null).
fn ffi_ret_to_json(ret: &crate::det::FfiRet) -> (&'static str, serde_json::Value) {
    use crate::det::FfiRet;
    use serde_json::json;
    match ret {
        FfiRet::Int(n) => ("int", json!(n)),
        FfiRet::Float(f) => ("float", json!(f)),
        FfiRet::Void => ("void", serde_json::Value::Null),
    }
}

/// Parse a JSON value that is a number array of bytes back into `Vec<u8>` (the
/// boundary-event byte encoding). A non-array / out-of-range entry yields an empty
/// vec (best-effort; the runtime replay-mismatch guard is authoritative).
fn json_bytes(v: Option<&serde_json::Value>) -> Vec<u8> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|n| n.as_u64())
                .map(|n| n as u8)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the newline-JSON log back into a `DetEvent` stream (resume). A malformed
/// line is skipped (best-effort: the runtime replay-mismatch detector is the
/// authoritative guard).
fn log_to_events(text: &str) -> Vec<DetEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = rec.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        match kind {
            "ClockRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::ClockRead { value: v });
                }
            }
            "MonotonicRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::MonotonicRead { value: v });
                }
            }
            "RandomRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::RandomRead { value: v });
                }
            }
            // Task 0.19c: a seeded byte draw — decode the JSON number array back to bytes.
            "BytesRead" => {
                events.push(DetEvent::BytesRead {
                    bytes: json_bytes(rec.get("bytes")),
                });
            }
            "TimerSet" => {
                if let Some(w) = rec.get("wake").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::TimerSet { wake: w });
                }
            }
            "ActivityCompleted" => {
                let name = rec
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_hash = rec
                    .get("argsHash")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let result_json = rec
                    .get("result")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string());
                events.push(DetEvent::ActivityCompleted {
                    name,
                    args_hash,
                    result_json,
                });
            }
            "ActorCall" => {
                let method = rec
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let result = json_bytes(rec.get("result"));
                let panic = rec
                    .get("panic")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                events.push(DetEvent::ActorCall {
                    method,
                    result,
                    panic,
                });
            }
            "GeneratorYield" => {
                let value = rec
                    .get("value")
                    .filter(|v| !v.is_null())
                    .map(|v| json_bytes(Some(v)));
                let panic = rec
                    .get("panic")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                events.push(DetEvent::GeneratorYield { value, panic });
            }
            // FFI Task 10 (§7A): a recorded foreign call — the marshalled return + the
            // post-call `Bytes` out-param snapshots.
            "FfiCall" => {
                let ret = ffi_ret_from_json(&rec);
                let out_params = rec
                    .get("outParams")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|pair| {
                                let p = pair.as_array()?;
                                let idx = p.first()?.as_u64()? as usize;
                                let bytes = json_bytes(p.get(1));
                                Some((idx, bytes))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                events.push(DetEvent::FfiCall { ret, out_params });
            }
            _ => {}
        }
    }
    events
}

/// FFI Task 10: decode an [`FfiRet`] from a `FfiCall` JSON log record (`retKind` +
/// `retValue`). A malformed/missing record decodes to `Void` (best-effort — the
/// runtime replay path tolerates it; a real recorded call always writes a valid tag).
fn ffi_ret_from_json(rec: &serde_json::Value) -> crate::det::FfiRet {
    use crate::det::FfiRet;
    match rec.get("retKind").and_then(|v| v.as_str()) {
        Some("int") => FfiRet::Int(rec.get("retValue").and_then(|v| v.as_i64()).unwrap_or(0)),
        Some("float") => {
            FfiRet::Float(rec.get("retValue").and_then(|v| v.as_f64()).unwrap_or(0.0))
        }
        _ => FfiRet::Void,
    }
}

/// WARM C (§4.2): the parsed durability policy. Default is `Fsync` (today's behavior,
/// unchanged). `Group` is parsed and validated here; its per-event-append behavior lands
/// in Task 10 — for now it is treated as the `Fsync` path with a clear TODO.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Durability {
    /// Whole-log snapshot at finish, F_FULLFSYNC + dir-fsync per commit (default).
    Fsync,
    /// Per-event append + coalesced fsync (Task 10 — group appender not yet wired).
    Group { window_ms: f64, max_events: usize },
    /// Whole-log snapshot at finish, no explicit fsync (OS-asynchronous writeback).
    Buffered,
}

/// Read `{log: "path", durability?: "fsync"|"buffered"|"group", groupWindowMs?, groupMaxEvents?}`
/// from a workflow options Object, returning `(log_path, durability)`. `log` is required.
///
/// WARM C §4.2 hardening: an UNKNOWN `durability` string is a Tier-2 error naming the
/// three valid values. Previously, anything other than `"buffered"` silently meant fsync;
/// a typo like `"groop"` now errors rather than silently choosing a different durability class.
fn read_options(opts: &Value, span: Span) -> Result<(String, Durability), Control> {
    let ValueKind::Object(o) = opts.kind() else {
        return Err(AsError::at(
            "workflow: options must be an object with a `log` path",
            span,
        )
        .into());
    };
    let log = match o.get("log").as_ref().map(Value::kind) {
        Some(ValueKind::Str(s)) => s.to_string(),
        _ => {
            return Err(AsError::at(
                "workflow: options.log must be a string file path",
                span,
            )
            .into())
        }
    };
    let durability = match o.get("durability").as_ref().map(Value::kind) {
        None | Some(ValueKind::Nil) => Durability::Fsync,
        Some(ValueKind::Str(s)) => match s.as_ref() {
            "fsync" => Durability::Fsync,
            "buffered" => Durability::Buffered,
            "group" => {
                // Parse optional override parameters with defaults (window=50ms, max=128).
                let window_ms = match o.get("groupWindowMs").as_ref().map(Value::kind) {
                    None | Some(ValueKind::Nil) => 50.0_f64,
                    Some(ValueKind::Int(n)) => n as f64,
                    Some(ValueKind::Float(f)) => f,
                    _ => {
                        return Err(AsError::at(
                            "workflow: groupWindowMs must be a number",
                            span,
                        )
                        .into())
                    }
                };
                if window_ms <= 0.0 || !window_ms.is_finite() {
                    return Err(AsError::at(
                        "workflow: groupWindowMs must be a positive finite number",
                        span,
                    )
                    .into());
                }
                let max_events = match o.get("groupMaxEvents").as_ref().map(Value::kind) {
                    None | Some(ValueKind::Nil) => 128_usize,
                    Some(ValueKind::Int(n)) => {
                        if n <= 0 {
                            return Err(AsError::at(
                                "workflow: groupMaxEvents must be a positive integer",
                                span,
                            )
                            .into());
                        }
                        n as usize
                    }
                    Some(ValueKind::Float(f)) => {
                        let n = f as i64;
                        if n <= 0 || !f.is_finite() {
                            return Err(AsError::at(
                                "workflow: groupMaxEvents must be a positive integer",
                                span,
                            )
                            .into());
                        }
                        n as usize
                    }
                    _ => {
                        return Err(AsError::at(
                            "workflow: groupMaxEvents must be a positive integer",
                            span,
                        )
                        .into())
                    }
                };
                Durability::Group { window_ms, max_events }
            }
            other => {
                return Err(AsError::at(
                    format!(
                        "workflow: unknown durability '{}' — valid values are 'fsync', 'group', 'buffered'",
                        other
                    ),
                    span,
                )
                .into())
            }
        },
        _ => {
            return Err(AsError::at(
                "workflow: durability must be a string ('fsync', 'group', or 'buffered')",
                span,
            )
            .into())
        }
    };
    Ok((log, durability))
}

impl Interp {
    /// `std/workflow` dispatch (the `workflow.*` builtins). All three entry points
    /// take `&self` so they reach the per-`Interp` determinism context + can call
    /// user closures via `call_value`.
    pub(crate) async fn call_workflow(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // `activity(name, fn)` → a tagged Object wrapping the side-effecting fn.
            // A bare `activity(...)` outside a workflow is just a callable record;
            // recording happens only via `ctx.call`.
            "activity" => {
                let name = match args.first().map(Value::kind) {
                    Some(ValueKind::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            "workflow.activity(name, fn): name must be a string",
                            span,
                        )
                        .into())
                    }
                };
                let func_val = args.get(1).cloned().unwrap_or(Value::nil());
                if !is_callable(&func_val) {
                    return Err(AsError::at(
                        "workflow.activity(name, fn): fn must be a function",
                        span,
                    )
                    .into());
                }
                Ok(make_activity(name, func_val))
            }
            "run" => self.workflow_run(args, span, /*replay=*/ false).await,
            "resume" => self.workflow_run(args, span, /*replay=*/ true).await,
            _ => Err(AsError::at(format!("std/workflow has no function '{}'", func), span).into()),
        }
    }

    /// Shared `run`/`resume` driver. `replay=false` (run): enter Record mode with a
    /// fresh log. `replay=true` (resume): if the log ends in a completion, return the
    /// recorded result (idempotent); else enter Replay mode primed from the log and
    /// re-run the workflow, fast-forwarding through recorded effects and recording any
    /// new ones from the crash point.
    async fn workflow_run(
        &self,
        args: &[Value],
        span: Span,
        replay: bool,
    ) -> Result<Value, Control> {
        let wf = args.first().cloned().unwrap_or(Value::nil());
        if !is_callable(&wf) {
            return Err(AsError::at("workflow.run/resume: first arg must be a function", span).into());
        }
        let input = args.get(1).cloned().unwrap_or(Value::nil());
        let opts = args.get(2).cloned().unwrap_or(Value::nil());
        let (log_path, durability) = read_options(&opts, span)?;

        // Seed the determinism context from the log path so a record/resume pair on
        // the same log uses the same RNG seed (deterministic across the boundary).
        let seed = crate::det::deterministic_start_ms(log_path_seed(&log_path)) as u64;
        let start_ms = crate::interp::real_now_ms();

        if replay {
            // Idempotent completion check FIRST (both modes): if the log already holds a
            // recorded WorkflowCompleted result line, return it without re-running. For
            // the group path we read the REPAIRED prefix so a torn tail after a completion
            // does not hide the completion (and a torn tail before one is truncated away).
            let (recorded, completed, repaired_file) =
                if matches!(durability, Durability::Group { .. }) {
                    // Open + repair (prefix-truncate the torn tail) before reading.
                    let (file, prefix_text) = open_group_log(&log_path, span)?;
                    let completed = completed_result(&prefix_text);
                    (log_to_events(&prefix_text), completed, Some(file))
                } else {
                    let existing = std::fs::read_to_string(&log_path).unwrap_or_default();
                    (log_to_events(&existing), completed_result(&existing), None)
                };
            if let Some(result) = completed {
                return Ok(result);
            }
            let persisted = recorded.len();
            let mut ctx = DeterminismContext::replay(seed, start_ms, recorded);
            if let (Durability::Group { window_ms, max_events }, Some(file)) =
                (durability, repaired_file)
            {
                // Seed `persisted` to the repaired-prefix count so only NEW events append.
                ctx.set_group_appender(crate::det::GroupAppender::new(
                    file,
                    persisted,
                    window_ms,
                    max_events,
                    group_record_line,
                ));
            }
            let prev = self.install_determinism(ctx);
            let outcome = self.drive_workflow(wf, input, span).await;
            self.finish_workflow(outcome, &log_path, durability, prev, span)
                .await
        } else {
            let mut ctx = DeterminismContext::record(seed, start_ms);
            if let Durability::Group { window_ms, max_events } = durability {
                // Fresh run: create/truncate the log, install the appender at persisted=0.
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&log_path)
                    .map_err(|e| {
                        AsError::at(
                            format!("workflow: cannot create group log '{}': {}", log_path, e),
                            span,
                        )
                    })?;
                ctx.set_group_appender(crate::det::GroupAppender::new(
                    file,
                    0,
                    window_ms,
                    max_events,
                    group_record_line,
                ));
            }
            let prev = self.install_determinism(ctx);
            let outcome = self.drive_workflow(wf, input, span).await;
            self.finish_workflow(outcome, &log_path, durability, prev, span)
                .await
        }
    }

    /// Build the `ctx`, call `wf(ctx, input)`, and await its result if it is async.
    async fn drive_workflow(
        &self,
        wf: Value,
        input: Value,
        span: Span,
    ) -> Result<Value, Control> {
        let ctx = make_ctx();
        let result = self.call_value(wf, vec![ctx, input], span).await?;
        // A workflow body may be `async fn` (returns a Future) — await it.
        if matches!(result.kind(), ValueKind::Future(_)) {
            let OwnedKind::Future(f) = result.into_kind() else {
                unreachable!()
            };
            f.get().await
        } else {
            Ok(result)
        }
    }

    /// Persist the event log + restore the previous determinism context, then return
    /// the workflow result (or propagate its error). On success the terminal
    /// `WorkflowCompleted` line is appended so a later `resume` is idempotent.
    async fn finish_workflow(
        &self,
        outcome: Result<Value, Control>,
        log_path: &str,
        durability: Durability,
        prev: Option<DeterminismContext>,
        span: Span,
    ) -> Result<Value, Control> {
        let mut ctx = self.take_determinism();

        // WARM C (§4.3): the GROUP path persists events incrementally as they are
        // recorded (the appender), so finish does NOT snapshot via `write_log`. It only
        // appends the terminal `WorkflowCompleted` line (on success) through the appender
        // and does a final DEADLINE-CHECKED `maybe_fsync` (NOT a forced fsync — that would
        // reinstate the per-commit F_FULLFSYNC and forfeit the bench win). The appender is
        // then dropped (file closed).
        let is_group = ctx
            .as_ref()
            .map(|c| c.has_group_appender())
            .unwrap_or(false);
        if is_group {
            let terminal = match &outcome {
                Ok(result) => group_completion_line(result),
                // On error, no completion record — the partial log is replayable.
                Err(_) => Vec::new(),
            };
            if let Some(c) = ctx.as_mut() {
                if let Err(e) = c.finish_group(&terminal) {
                    self.restore_determinism(prev);
                    return Err(AsError::at(
                        format!("workflow: group log finish failed: {}", e),
                        span,
                    )
                    .into());
                }
            }
            self.restore_determinism(prev);
            return outcome;
        }

        self.restore_determinism(prev);
        let events = ctx.map(|c| c.events).unwrap_or_default();
        // Fsync/Buffered (UNCHANGED, byte-identical to pre-WARM): always flush the
        // recorded effect stream as a whole-log atomic snapshot at finish (even on
        // error: a crash mid-run leaves a partial log a later resume fast-forwards
        // through).
        let mut log = events_to_log(&events);
        if let Ok(ref result) = outcome {
            // Append the terminal completion record so `resume` is idempotent.
            let rec = serde_json::json!({
                "kind": "WorkflowCompleted",
                "result": serde_json::from_str::<serde_json::Value>(&to_json_string(result))
                    .unwrap_or(serde_json::Value::Null),
            });
            log.push_str(&rec.to_string());
            log.push('\n');
        }
        // - Fsync: snapshot-at-finish + F_FULLFSYNC (unchanged behavior).
        // - Buffered: snapshot-at-finish, no explicit fsync (unchanged behavior).
        // (Group never reaches here — handled above.)
        let fsync = !matches!(durability, Durability::Buffered);
        write_log(log_path, &log, fsync, span)?;
        outcome
    }

    /// `ctx.<method>(...)` dispatch (routed here by the call-site hook in both
    /// engines when the receiver is the workflow-context tagged Object). Outside a
    /// workflow (no determinism context) these are an error — `ctx` only exists
    /// inside a `run`/`resume`.
    pub(crate) async fn call_workflow_ctx(
        &self,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        if !self.is_deterministic() {
            return Err(AsError::at(
                format!("workflow ctx.{} called outside a workflow", name),
                span,
            )
            .into());
        }
        // args[0] is the receiver ctx (pushed by the hook); the user args follow.
        let user = &args[1..];
        match name {
            "now" => Ok(Value::float(self.clock_now_ms())),
            "random" => Ok(Value::float(self.next_seeded_f64().unwrap_or(0.0))),
            "uuid" => {
                let mut bytes = [0u8; 16];
                self.fill_seeded_bytes(&mut bytes);
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                Ok(Value::str(uuid::Uuid::from_bytes(bytes).to_string()))
            }
            "sleep" => {
                let ms = user.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
                // Durable timer: advance the virtual clock + record a TimerSet; no
                // real delay. On resume the recorded TimerSet is consumed and the
                // clock fast-forwards.
                self.with_determinism_mut(|c| {
                    if c.mode == Mode::Replay {
                        // Consume the recorded TimerSet (advance cursor) if present.
                        if let Some(DetEvent::TimerSet { wake }) = c.events.get(c.cursor).cloned() {
                            c.cursor += 1;
                            c.clock.set_now(wake);
                            return;
                        }
                        // No recorded timer (crash point): switch to record.
                        c.mode = Mode::Record;
                    }
                    c.clock.advance(ms);
                    let wake = c.clock.now_ms();
                    let _ = c.record_event(DetEvent::TimerSet { wake });
                });
                Ok(Value::nil())
            }
            "call" => self.ctx_call_activity(user, span).await,
            _ => Err(AsError::at(format!("workflow ctx has no method '{}'", name), span).into()),
        }
    }

    /// `ctx.call(activity, ...args)`: record-or-replay an activity result by its
    /// sequence position. Record: run the activity fn for real, JSON-serialize the
    /// result, append `ActivityCompleted`. Replay: consume the next recorded event,
    /// assert its signature matches (else a non-determinism panic), and return its
    /// result WITHOUT executing the side effect; on a missing event (the crash
    /// point) switch to Record and execute for real.
    async fn ctx_call_activity(&self, user: &[Value], span: Span) -> Result<Value, Control> {
        let activity = user.first().cloned().unwrap_or(Value::nil());
        if !is_activity_value(&activity) {
            return Err(AsError::at(
                "workflow ctx.call(activity, ...): first arg must be an activity",
                span,
            )
            .into());
        }
        let act_args: Vec<Value> = user.get(1..).map(|s| s.to_vec()).unwrap_or_default();
        let (act_name, act_fn) = activity_parts(&activity);
        let sig = signature_hash(&act_name, &act_args);

        // Replay: try to consume a recorded ActivityCompleted at the cursor.
        let replay_hit = self.with_determinism_mut(|c| {
            if c.mode != Mode::Replay {
                return Ok::<Option<Value>, String>(None);
            }
            match c.events.get(c.cursor).cloned() {
                Some(DetEvent::ActivityCompleted {
                    name,
                    args_hash,
                    result_json,
                }) => {
                    // Replay-mismatch detection: the recorded signature must match.
                    if name != act_name || args_hash != sig {
                        return Err(format!(
                            "workflow non-determinism: expected activity '{}' at seq {}, got '{}'",
                            name, c.cursor, act_name
                        ));
                    }
                    c.cursor += 1;
                    Ok(Some(from_json_string(&result_json)))
                }
                Some(_) => Err(format!(
                    "workflow non-determinism: expected activity '{}' at seq {}, got a non-activity event",
                    act_name, c.cursor
                )),
                None => {
                    // Crash point: nothing recorded here → switch to record + run.
                    c.mode = Mode::Record;
                    Ok(None)
                }
            }
        });
        match replay_hit {
            Some(Ok(Some(v))) => return Ok(v), // replayed, no side effect
            Some(Err(msg)) => return Err(AsError::at(msg, span).into()),
            Some(Ok(None)) | None => {} // record path below
        }

        // Record path: run the activity for real, await if async, serialize result.
        let result = self.call_value(act_fn, act_args, span).await?;
        let result = if matches!(result.kind(), ValueKind::Future(_)) {
            let OwnedKind::Future(f) = result.into_kind() else {
                unreachable!()
            };
            f.get().await?
        } else {
            result
        };
        // Constraint: only Value-serializable results persist. A native handle /
        // function / class is a constraint violation at record time.
        if !is_serializable(&result) {
            return Err(AsError::at(
                "workflow: activity result is not serializable (return data, not a native handle/function/class)",
                span,
            )
            .into());
        }
        let result_json = to_json_string(&result);
        // WARM C (§4.3): route through the `record_event` chokepoint. Under group
        // durability this pumps the new `ActivityCompleted` to disk synchronously
        // (write-at-record-time — the kill-9 guarantee). An I/O error on the pump is the
        // durability-critical failure (§4.5 `ENOSPC`/`EIO`): surface it as a Tier-2 error
        // rather than continue believing the activity is durably recorded.
        let pump_err = self.with_determinism_mut(|c| {
            c.record_event(DetEvent::ActivityCompleted {
                name: act_name.clone(),
                args_hash: sig,
                result_json: result_json.clone(),
            })
        });
        if let Some(Err(e)) = pump_err {
            return Err(AsError::at(
                format!("workflow: durable log append failed: {}", e),
                span,
            )
            .into());
        }
        Ok(result)
    }
}

/// Extract `(name, fn)` from an activity tagged Object.
fn activity_parts(activity: &Value) -> (String, Value) {
    if let ValueKind::Object(o) = activity.kind() {
        let name = match o.get("name").as_ref().map(Value::kind) {
            Some(ValueKind::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let func = o.get("fn").unwrap_or(Value::nil());
        (name, func)
    } else {
        (String::new(), Value::nil())
    }
}

/// A callable value (function/closure/builtin/bound-method).
fn is_callable(v: &Value) -> bool {
    matches!(
        v.kind(),
        ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::NativeMethod(_)
    )
}

/// A `Value` the JSON codec round-trips for the durable log: data only, never a
/// live native handle / function / class.
fn is_serializable(v: &Value) -> bool {
    !matches!(
        v.kind(),
        ValueKind::Native(_)
            | ValueKind::NativeMethod(_)
            | ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::Class(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::Future(_)
            | ValueKind::Generator(_)
    )
}

/// A small deterministic seed from the log path so a record/resume pair on the same
/// log share an RNG seed.
fn log_path_seed(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// If the log text ends with a `WorkflowCompleted` record, return its result Value
/// (idempotent-resume short circuit).
fn completed_result(text: &str) -> Option<Value> {
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: serde_json::Value = serde_json::from_str(line).ok()?;
        if rec.get("kind").and_then(|k| k.as_str()) == Some("WorkflowCompleted") {
            let result = rec.get("result").cloned().unwrap_or(serde_json::Value::Null);
            return Some(crate::stdlib::json::to_ascript(&result));
        }
        // The last non-empty line was not a completion → not idempotent-done.
        return None;
    }
    None
}

/// Write the event log to disk **atomically**: write to a sibling temp file, fsync it
/// (when `fsync`), then `rename` it over the target. POSIX `rename` is atomic at the
/// directory level, so a crash (OOM/SIGKILL/power loss) at any instant leaves `path`
/// holding EITHER the previous complete log OR the new complete log — never a
/// zero-byte/partial file. This is the keystone of the exactly-once activity
/// guarantee: the old in-place `File::create(path)` truncated the target before
/// writing, so a crash mid-write corrupted the log and forced a full re-execution.
///
/// **Single-writer-per-log** (module contract): only one process/isolate writes a
/// given log path at a time (the replay model already depends on this). The temp
/// sibling is pid-qualified (`<path>.<pid>.tmp`) so two unrelated processes targeting
/// the same path don't clobber each other's in-flight temp; concurrent writers within
/// the SAME process are not supported by design.
///
/// **Durability:** when `fsync`, the temp file's data is fsync'd before the rename and
/// the parent directory is fsync'd AFTER the rename, so the directory entry update
/// (the rename itself) is durable — without that, a crash could lose the rename even
/// though the file data was synced. (POSIX; on platforms where opening the directory
/// for fsync is unsupported, the dir-sync is a best-effort no-op.)
fn write_log(path: &str, contents: &str, fsync: bool, span: Span) -> Result<(), Control> {
    use std::io::Write;
    let tmp = format!("{}.{}.tmp", path, std::process::id());
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| AsError::at(format!("workflow: cannot write log '{}': {}", tmp, e), span))?;
    f.write_all(contents.as_bytes())
        .map_err(|e| AsError::at(format!("workflow: log write failed: {}", e), span))?;
    if fsync {
        // The user explicitly opted into durability — a failed fsync (ENOSPC/EIO)
        // means the data is NOT durable, so surface it rather than rename-and-lie.
        f.sync_all()
            .map_err(|e| AsError::at(format!("workflow: log sync failed: {}", e), span))?;
    }
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| {
        // Clean up the orphaned temp so a failed commit doesn't leave litter.
        let _ = std::fs::remove_file(&tmp);
        AsError::at(format!("workflow: log commit failed: {}", e), span)
    })?;
    if fsync {
        // Fsync the parent directory so the rename (a directory-entry change) is itself
        // durable. Best-effort: not all platforms allow opening a dir for sync.
        if let Some(parent) = std::path::Path::new(path).parent() {
            let dir = if parent.as_os_str().is_empty() {
                std::path::Path::new(".")
            } else {
                parent
            };
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
    }
    Ok(())
}

// Keep `Rc`/`RefCell` imports used (the tagged-object constructors build cells).
const _: fn() = || {
    let _ = Rc::new(RefCell::new(0u8));
};

#[cfg(test)]
mod write_log_tests {
    use super::write_log;
    use crate::span::Span;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ascript_wflog_{}_{}_{:?}",
            name,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A successful atomic write leaves the complete content at `path` and NO stray
    /// `.tmp` sibling behind.
    #[test]
    fn success_writes_complete_content_and_leaves_no_tmp() {
        let dir = tmp_dir("ok");
        let path = dir.join("events.log");
        let p = path.to_string_lossy().into_owned();

        write_log(&p, "first\n", false, Span::new(0, 0)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");

        write_log(&p, "first\nsecond\n", false, Span::new(0, 0)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        // No `.tmp` sibling may linger after a committed rename.
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "the .tmp sibling must be renamed away on success; found {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `fsync=true` durability path (the production default `durability:"fsync"`)
    /// completes the full write → file-fsync → rename → dir-fsync sequence, leaving the
    /// correct content and no stray `.tmp`. This is the regression guard for surfacing
    /// the file `sync_all()` error: the success branch must still return `Ok` and
    /// commit. (The only prior fsync=true test failed at `File::create` and never
    /// reached the sync, so this branch was untested.)
    #[test]
    fn fsync_true_success_path_commits_correct_content() {
        let dir = tmp_dir("fsync_ok");
        let path = dir.join("events.log");
        let p = path.to_string_lossy().into_owned();

        write_log(&p, "durable-line-1\n", true, Span::new(0, 0)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "durable-line-1\n");

        // Overwrite atomically again with fsync on.
        write_log(&p, "durable-line-1\ndurable-line-2\n", true, Span::new(0, 0)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "durable-line-1\ndurable-line-2\n"
        );

        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "fsync path must rename the .tmp away; found {strays:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the commit cannot proceed (here: the parent directory is read-only so the
    /// temp sibling can't even be created), the ORIGINAL log is left fully intact.
    /// The old `File::create(path)` truncated the target BEFORE writing, so this
    /// property failed for it; temp+rename never touches `path` until the atomic
    /// rename. Unix-only (read-only-dir enforcement is reliable on POSIX).
    #[cfg(unix)]
    #[test]
    fn failure_leaves_original_untouched() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp_dir("fail");
        let path = dir.join("events.log");

        // Seed a valid previous log.
        std::fs::write(&path, "old-complete-log\n").unwrap();

        // Make the directory read-only so creating the temp sibling fails AND (with
        // the old code) re-creating `path` would also have to truncate it first.
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o500); // r-x------ : no write/create in the dir
        std::fs::set_permissions(&dir, perms).unwrap();

        let p = path.to_string_lossy().into_owned();
        let res = write_log(&p, "brand-new-content\n", true, Span::new(0, 0));

        // Restore write perms so we can read/clean up regardless of outcome.
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&dir, perms).unwrap();

        // The temp-then-rename approach cannot create the `.tmp` sibling in a
        // read-only directory, so the commit fails cleanly. The OLD in-place
        // `File::create(path)` truncated and rewrote the EXISTING file (no directory
        // entry created → permitted even in a read-only dir), silently destroying the
        // prior log. The decisive guarantee: the original log is never destroyed by a
        // write that did not fully commit.
        assert!(res.is_err(), "write into a read-only dir must error cleanly");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "old-complete-log\n",
            "a failed write must NOT corrupt or truncate the existing log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Task 0.19c: the `BytesRead` det-event survives the newline-JSON log codec
/// (`events_to_log` → `log_to_events`) byte-for-byte. Guards the serde path the
/// end-to-end workflow replay depends on.
#[cfg(test)]
mod bytes_read_log_codec_tests {
    use super::{events_to_log, log_to_events};
    use crate::det::DetEvent;

    #[test]
    fn bytes_read_round_trips_through_the_log() {
        let events = vec![
            DetEvent::BytesRead {
                bytes: vec![0, 1, 127, 128, 255, 16],
            },
            DetEvent::RandomRead { value: 0.25 },
            DetEvent::BytesRead { bytes: vec![] }, // empty draw is faithful too
        ];
        let log = events_to_log(&events);
        assert!(log.contains("\"kind\":\"BytesRead\""), "log must carry the kind tag");
        let back = log_to_events(&log);
        assert_eq!(back, events, "BytesRead must round-trip the exact bytes");
    }
}
