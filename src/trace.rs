//! REPLAY §3 — the `ASTRC` trace container: a versioned, length-prefixed,
//! CRC-checked binary format for an `ascript run --record/--replay` session.
//!
//! A SIBLING discipline to `.aso` (`src/vm/aso.rs`) — NOT a change to it. The
//! trace is an attacker-writable file, so [`read_trace`] is a SECURITY surface:
//! every length is bounds-checked against the remaining buffer, every record's
//! crc32 is verified, an unknown version / record-kind / a truncation at any
//! offset is a clean [`TraceError`] naming the failing record index — never a
//! panic, never an unbounded allocation, never an unchecked slice.
//!
//! This module is CORE: no serde, no feature gate. The [`DetEvent`] /
//! [`TraceOutcome`] / [`FfiRet`] enums it encodes live in `src/det.rs` (also
//! core, no `Value`/serde dependency) — every variant and every field
//! round-trips. The encode is hand-rolled (a [`Writer`]/[`Reader`] pair
//! mirroring `aso.rs`'s discipline) precisely so it builds under
//! `--no-default-features`.
//!
//! Format (spec §3, verbatim):
//! ```text
//! magic       8   b"ASTRC\0\0\0"
//! version     u16 TRACE_FORMAT_VERSION = 1
//! header_len  u32
//! header          seed u64 · start_ms f64 · kind u8 (0=run, 1=test)
//!                 · program path (len-prefixed utf8) · source sha256 [32 bytes]
//!                 · argv (count + len-prefixed strings)
//!                 · for kind=test: test name + the effective --filter string
//!                 · created_ms f64 · engine tag u8 (informational)
//! header_crc  u32 (crc32 over the header bytes)
//! record*     kind u8 · payload_len u32 · payload · crc32(kind‖len‖payload)
//! end         u8 0xFF · u32 event_count
//! ```

use crate::det::{DetEvent, FfiRet, TraceOutcome};
use crate::error::AsError;
use std::path::Path;

/// The on-disk trace container format version. Bumped on ANY layout/encoding
/// change (the `.aso` `ASO_FORMAT_VERSION` discipline). A reader rejects a
/// trace whose stored version is newer than the one it was built with.
pub const TRACE_FORMAT_VERSION: u16 = 1;

/// The 8-byte leading magic: `b"ASTRC\0\0\0"`.
pub const TRACE_MAGIC: [u8; 8] = *b"ASTRC\0\0\0";

/// Whether the recorded session was a plain `run` or a `test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    /// `ascript run --record <trace> file.as` — a program run.
    Run,
    /// `ascript test --record <trace> file.as` — a test run; the header carries
    /// the test name + the effective `--filter` string.
    Test,
}

impl TraceKind {
    fn to_u8(self) -> u8 {
        match self {
            TraceKind::Run => 0,
            TraceKind::Test => 1,
        }
    }
    fn from_u8(b: u8) -> Option<TraceKind> {
        match b {
            0 => Some(TraceKind::Run),
            1 => Some(TraceKind::Test),
            _ => None,
        }
    }
}

/// The trace header — the run identity a replay verifies against before
/// consuming a single event. `source_sha256` guards against replaying a trace
/// recorded for a since-changed program (a shifted stream → confusing mid-run
/// divergence); `seed` re-seeds the RNG; `argv` is taken FROM the trace by
/// default.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceHeader {
    /// The RNG seed installed for the recorded run (OS entropy or `--seed N`).
    pub seed: u64,
    /// The virtual-clock start (ms epoch) — real time at record, so recorded
    /// timestamps look real.
    pub start_ms: f64,
    /// Whether this was a `run` or a `test`.
    pub kind: TraceKind,
    /// The recorded program's path.
    pub program_path: String,
    /// sha256 of the recorded program source — the replay identity check.
    pub source_sha256: [u8; 32],
    /// The script args the program ran with.
    pub argv: Vec<String>,
    /// For [`TraceKind::Test`]: the test name. `None` for a `run`.
    pub test_name: Option<String>,
    /// For [`TraceKind::Test`]: the effective `--filter` string. `None` for a
    /// `run` (only meaningful for a test).
    pub filter: Option<String>,
    /// Wall-clock ms when the trace file was written (informational).
    pub created_ms: f64,
    /// The engine the run used (informational): an opaque tag.
    pub engine: u8,
}

/// An error from decoding an `ASTRC` byte stream. Every variant is a CLEAN,
/// hostile-input rejection (never a panic). The Display strings follow the
/// project style guide (lowercase, no trailing period) and name the failing
/// record index where applicable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceError {
    /// The leading 8 bytes were not [`TRACE_MAGIC`].
    BadMagic,
    /// The stored version is newer than this binary supports.
    VersionTooNew { found: u16, max: u16 },
    /// The byte stream ended before a field could be fully read. `record` is
    /// the 0-based record index being decoded, or `None` for the header.
    Truncated { record: Option<usize> },
    /// A crc32 mismatch. `record` is the 0-based record index, or `None` for
    /// the header crc.
    BadCrc { record: Option<usize> },
    /// A record's `kind` byte did not name a known [`DetEvent`] variant.
    UnknownRecordKind { record: usize, kind: u8 },
    /// A discriminant byte inside a record payload (an outcome tag, an option
    /// flag, an ffi-ret tag, a trace-kind byte) was out of range.
    BadTag {
        record: Option<usize>,
        what: &'static str,
        tag: u8,
    },
    /// A UTF-8 string field was not valid UTF-8.
    BadUtf8 { record: Option<usize> },
    /// A `u64`/`u32` length did not fit the host `usize`.
    Overflow { record: Option<usize> },
    /// The end marker (`0xFF` + count) was missing, malformed, or the stored
    /// `event_count` did not equal the number of records actually read.
    BadEndMarker,
    /// Trailing bytes remained after the end marker.
    TrailingBytes,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn at(record: &Option<usize>) -> String {
            match record {
                Some(n) => format!("at record {n}"),
                None => "in the header".to_string(),
            }
        }
        match self {
            TraceError::BadMagic => write!(f, "not a trace file (bad magic, expected 'ASTRC')"),
            TraceError::VersionTooNew { found, max } => write!(
                f,
                "trace version {found} is newer than this binary supports (max {max})"
            ),
            TraceError::Truncated { record } => {
                write!(f, "trace file is corrupt or truncated {}", at(record))
            }
            TraceError::BadCrc { record } => {
                write!(f, "trace file has a checksum mismatch {}", at(record))
            }
            TraceError::UnknownRecordKind { record, kind } => {
                write!(f, "unknown trace record kind {kind} at record {record}")
            }
            TraceError::BadTag { record, what, tag } => {
                write!(f, "invalid {what} tag {tag} {}", at(record))
            }
            TraceError::BadUtf8 { record } => {
                write!(f, "invalid UTF-8 in trace string field {}", at(record))
            }
            TraceError::Overflow { record } => {
                write!(f, "trace length exceeds host usize {}", at(record))
            }
            TraceError::BadEndMarker => {
                write!(f, "trace file is missing or has a malformed end marker")
            }
            TraceError::TrailingBytes => write!(f, "trailing bytes after the trace end marker"),
        }
    }
}

impl std::error::Error for TraceError {}

// ===========================================================================
// CRC32 (IEEE 802.3, reflected) — hand-rolled, table-driven, no new dep.
// ===========================================================================

/// The reflected IEEE 802.3 crc32 of `data`. Standard polynomial `0xEDB88320`,
/// init `0xFFFFFFFF`, final XOR `0xFFFFFFFF` — i.e. `crc32("123456789")`
/// `== 0xCBF43926` (the canonical check vector, pinned by a unit test).
fn crc32(data: &[u8]) -> u32 {
    // The 256-entry lookup table, built once on first use.
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    });

    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

// ===========================================================================
// Writer — a little-endian byte sink (mirrors aso.rs's `Writer`).
// ===========================================================================

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        // Bit pattern so NaN/-0.0 round-trip exactly.
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    /// A length-prefixed (`u32`) byte field. A field > `u32::MAX` is clamped to
    /// the placeholder length; the trace writer never produces such a field in
    /// practice (an airlock payload that large is refused upstream), and a hand-
    /// crafted hostile READ of a too-large declared length is the reader's job.
    fn bytes(&mut self, b: &[u8]) {
        self.u32(u32::try_from(b.len()).unwrap_or(u32::MAX));
        self.buf.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn opt_bytes(&mut self, b: Option<&[u8]>) {
        match b {
            Some(b) => {
                self.u8(1);
                self.bytes(b);
            }
            None => self.u8(0),
        }
    }
    /// A `usize` count widened to `u32` (record/argv counts).
    fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
}

// ===========================================================================
// Reader — a bounds-checked little-endian byte source (mirrors aso.rs).
// ===========================================================================

/// The record index the reader is currently decoding, threaded into every
/// [`TraceError`] so a truncation/crc/utf8 failure names the failing record.
/// `None` while reading the header.
type Cur = Option<usize>;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize, cur: Cur) -> Result<&'a [u8], TraceError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(TraceError::Overflow { record: cur })?;
        if end > self.buf.len() {
            return Err(TraceError::Truncated { record: cur });
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self, cur: Cur) -> Result<u8, TraceError> {
        Ok(self.take(1, cur)?[0])
    }
    fn u16(&mut self, cur: Cur) -> Result<u16, TraceError> {
        let b = self.take(2, cur)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self, cur: Cur) -> Result<u32, TraceError> {
        let b = self.take(4, cur)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self, cur: Cur) -> Result<u64, TraceError> {
        let b = self.take(8, cur)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn f64(&mut self, cur: Cur) -> Result<f64, TraceError> {
        Ok(f64::from_bits(self.u64(cur)?))
    }
    /// A `u32` length narrowed to `usize`. NEVER pre-allocates on this value —
    /// callers `take()` the bytes (which bounds-checks against the live buffer),
    /// so an attacker-controlled huge length fails as a clean `Truncated`, not
    /// an OOM.
    fn len(&mut self, cur: Cur) -> Result<usize, TraceError> {
        usize::try_from(self.u32(cur)?).map_err(|_| TraceError::Overflow { record: cur })
    }
    fn bytes(&mut self, cur: Cur) -> Result<Vec<u8>, TraceError> {
        let n = self.len(cur)?;
        Ok(self.take(n, cur)?.to_vec())
    }
    fn str(&mut self, cur: Cur) -> Result<String, TraceError> {
        let n = self.len(cur)?;
        let b = self.take(n, cur)?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| TraceError::BadUtf8 { record: cur })
    }
    fn opt_str(&mut self, cur: Cur) -> Result<Option<String>, TraceError> {
        match self.u8(cur)? {
            0 => Ok(None),
            1 => Ok(Some(self.str(cur)?)),
            tag => Err(TraceError::BadTag {
                record: cur,
                what: "opt-string",
                tag,
            }),
        }
    }
    fn opt_bytes(&mut self, cur: Cur) -> Result<Option<Vec<u8>>, TraceError> {
        match self.u8(cur)? {
            0 => Ok(None),
            1 => Ok(Some(self.bytes(cur)?)),
            tag => Err(TraceError::BadTag {
                record: cur,
                what: "opt-bytes",
                tag,
            }),
        }
    }
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }
}

// ===========================================================================
// Record kind tags + payload codec
// ===========================================================================

// One stable byte per `DetEvent` variant. NEVER reuse/reorder a tag (the `.aso`
// const-tag discipline) — append a new tag for a new variant.
const REC_CLOCK_READ: u8 = 1;
const REC_RANDOM_READ: u8 = 2;
const REC_BYTES_READ: u8 = 3;
const REC_MONOTONIC_READ: u8 = 4;
const REC_TIMER_SET: u8 = 5;
const REC_ACTIVITY_COMPLETED: u8 = 6;
const REC_ACTOR_CALL: u8 = 7;
const REC_FFI_CALL: u8 = 8;
const REC_GENERATOR_YIELD: u8 = 9;
const REC_STDLIB_CALL: u8 = 10;
const REC_NATIVE_CALL: u8 = 11;

// `TraceOutcome` tags.
const OUT_VALUE: u8 = 0;
const OUT_PANIC: u8 = 1;
const OUT_PROPAGATE: u8 = 2;
const OUT_HANDLE: u8 = 3;

// `FfiRet` tags.
const FFI_INT: u8 = 0;
const FFI_FLOAT: u8 = 1;
const FFI_VOID: u8 = 2;

fn write_outcome(w: &mut Writer, o: &TraceOutcome) {
    match o {
        TraceOutcome::Value(b) => {
            w.u8(OUT_VALUE);
            w.bytes(b);
        }
        TraceOutcome::Panic(s) => {
            w.u8(OUT_PANIC);
            w.str(s);
        }
        TraceOutcome::Propagate(b) => {
            w.u8(OUT_PROPAGATE);
            w.bytes(b);
        }
        TraceOutcome::Handle {
            kind_tag,
            vid,
            fields,
        } => {
            w.u8(OUT_HANDLE);
            w.u8(*kind_tag);
            w.u32(*vid);
            w.bytes(fields);
        }
    }
}

fn read_outcome(r: &mut Reader, cur: Cur) -> Result<TraceOutcome, TraceError> {
    match r.u8(cur)? {
        OUT_VALUE => Ok(TraceOutcome::Value(r.bytes(cur)?)),
        OUT_PANIC => Ok(TraceOutcome::Panic(r.str(cur)?)),
        OUT_PROPAGATE => Ok(TraceOutcome::Propagate(r.bytes(cur)?)),
        OUT_HANDLE => {
            let kind_tag = r.u8(cur)?;
            let vid = r.u32(cur)?;
            let fields = r.bytes(cur)?;
            Ok(TraceOutcome::Handle {
                kind_tag,
                vid,
                fields,
            })
        }
        tag => Err(TraceError::BadTag {
            record: cur,
            what: "outcome",
            tag,
        }),
    }
}

/// Encode ONE event's `kind` + payload bytes (payload only — the framing/crc is
/// added by [`write_trace`]). Returns `(kind, payload)`.
fn encode_event(ev: &DetEvent) -> (u8, Vec<u8>) {
    let mut w = Writer::new();
    let kind = match ev {
        DetEvent::ClockRead { value } => {
            w.f64(*value);
            REC_CLOCK_READ
        }
        DetEvent::RandomRead { value } => {
            w.f64(*value);
            REC_RANDOM_READ
        }
        DetEvent::BytesRead { bytes } => {
            w.bytes(bytes);
            REC_BYTES_READ
        }
        DetEvent::MonotonicRead { value } => {
            w.f64(*value);
            REC_MONOTONIC_READ
        }
        DetEvent::TimerSet { wake } => {
            w.f64(*wake);
            REC_TIMER_SET
        }
        DetEvent::ActivityCompleted {
            name,
            args_hash,
            result_json,
        } => {
            w.str(name);
            w.u64(*args_hash);
            w.str(result_json);
            REC_ACTIVITY_COMPLETED
        }
        DetEvent::ActorCall {
            method,
            result,
            panic,
        } => {
            w.str(method);
            w.bytes(result);
            w.opt_str(panic.as_deref());
            REC_ACTOR_CALL
        }
        DetEvent::FfiCall { ret, out_params } => {
            match ret {
                FfiRet::Int(v) => {
                    w.u8(FFI_INT);
                    w.u64(*v as u64);
                }
                FfiRet::Float(v) => {
                    w.u8(FFI_FLOAT);
                    w.f64(*v);
                }
                FfiRet::Void => w.u8(FFI_VOID),
            }
            w.count(out_params.len());
            for (idx, bytes) in out_params {
                w.u64(*idx as u64);
                w.bytes(bytes);
            }
            REC_FFI_CALL
        }
        DetEvent::GeneratorYield { value, panic } => {
            w.opt_bytes(value.as_deref());
            w.opt_str(panic.as_deref());
            REC_GENERATOR_YIELD
        }
        DetEvent::StdlibCall {
            module,
            func,
            args_hash,
            outcome,
        } => {
            w.str(module);
            w.str(func);
            w.u64(*args_hash);
            write_outcome(&mut w, outcome);
            REC_STDLIB_CALL
        }
        DetEvent::NativeCall {
            vid,
            method,
            args_hash,
            outcome,
        } => {
            w.u32(*vid);
            w.str(method);
            w.u64(*args_hash);
            write_outcome(&mut w, outcome);
            REC_NATIVE_CALL
        }
    };
    (kind, w.buf)
}

/// Decode ONE event from its `kind` + a payload reader. The payload reader is
/// bounded to EXACTLY this record's payload bytes (sliced by [`read_trace`]),
/// so a per-field over-read is a clean `Truncated{record}` and trailing payload
/// bytes are caught as a clean error.
fn decode_event(kind: u8, r: &mut Reader, cur: usize) -> Result<DetEvent, TraceError> {
    let c = Some(cur);
    let ev = match kind {
        REC_CLOCK_READ => DetEvent::ClockRead { value: r.f64(c)? },
        REC_RANDOM_READ => DetEvent::RandomRead { value: r.f64(c)? },
        REC_BYTES_READ => DetEvent::BytesRead {
            bytes: r.bytes(c)?,
        },
        REC_MONOTONIC_READ => DetEvent::MonotonicRead { value: r.f64(c)? },
        REC_TIMER_SET => DetEvent::TimerSet { wake: r.f64(c)? },
        REC_ACTIVITY_COMPLETED => {
            let name = r.str(c)?;
            let args_hash = r.u64(c)?;
            let result_json = r.str(c)?;
            DetEvent::ActivityCompleted {
                name,
                args_hash,
                result_json,
            }
        }
        REC_ACTOR_CALL => {
            let method = r.str(c)?;
            let result = r.bytes(c)?;
            let panic = r.opt_str(c)?;
            DetEvent::ActorCall {
                method,
                result,
                panic,
            }
        }
        REC_FFI_CALL => {
            let ret = match r.u8(c)? {
                FFI_INT => FfiRet::Int(r.u64(c)? as i64),
                FFI_FLOAT => FfiRet::Float(r.f64(c)?),
                FFI_VOID => FfiRet::Void,
                tag => {
                    return Err(TraceError::BadTag {
                        record: c,
                        what: "ffi-ret",
                        tag,
                    })
                }
            };
            let n = r.len(c)?;
            // NO pre-allocation on `n` — each iteration reads ≥ 9 bytes from the
            // live buffer; a bomb count exceeding the payload fails as a clean
            // `Truncated` on the first short read. Cap the reserve at remaining.
            let mut out_params = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                let idx = usize::try_from(r.u64(c)?)
                    .map_err(|_| TraceError::Overflow { record: c })?;
                let bytes = r.bytes(c)?;
                out_params.push((idx, bytes));
            }
            DetEvent::FfiCall { ret, out_params }
        }
        REC_GENERATOR_YIELD => {
            let value = r.opt_bytes(c)?;
            let panic = r.opt_str(c)?;
            DetEvent::GeneratorYield { value, panic }
        }
        REC_STDLIB_CALL => {
            let module = r.str(c)?;
            let func = r.str(c)?;
            let args_hash = r.u64(c)?;
            let outcome = read_outcome(r, c)?;
            DetEvent::StdlibCall {
                module,
                func,
                args_hash,
                outcome,
            }
        }
        REC_NATIVE_CALL => {
            let vid = r.u32(c)?;
            let method = r.str(c)?;
            let args_hash = r.u64(c)?;
            let outcome = read_outcome(r, c)?;
            DetEvent::NativeCall {
                vid,
                method,
                args_hash,
                outcome,
            }
        }
        _ => return Err(TraceError::UnknownRecordKind { record: cur, kind }),
    };
    // A well-formed payload is consumed EXACTLY; trailing payload bytes are a
    // corrupt/forward-incompatible record.
    if !r.at_end() {
        return Err(TraceError::Truncated { record: c });
    }
    Ok(ev)
}

// ===========================================================================
// Header codec
// ===========================================================================

/// Encode the header BODY (the bytes the header_crc covers, between header_len
/// and header_crc) — NOT including the magic/version/header_len framing.
fn encode_header_body(h: &TraceHeader) -> Vec<u8> {
    let mut w = Writer::new();
    w.u64(h.seed);
    w.f64(h.start_ms);
    w.u8(h.kind.to_u8());
    w.str(&h.program_path);
    w.buf.extend_from_slice(&h.source_sha256);
    w.count(h.argv.len());
    for a in &h.argv {
        w.str(a);
    }
    // Test-only fields are always serialized (1-byte present flag each) so the
    // layout is fixed regardless of kind; only meaningful for `Test`.
    w.opt_str(h.test_name.as_deref());
    w.opt_str(h.filter.as_deref());
    w.f64(h.created_ms);
    w.u8(h.engine);
    w.buf
}

fn decode_header_body(body: &[u8]) -> Result<TraceHeader, TraceError> {
    let mut r = Reader::new(body);
    let cur = None;
    let seed = r.u64(cur)?;
    let start_ms = r.f64(cur)?;
    let kind_byte = r.u8(cur)?;
    let kind = TraceKind::from_u8(kind_byte).ok_or(TraceError::BadTag {
        record: cur,
        what: "trace-kind",
        tag: kind_byte,
    })?;
    let program_path = r.str(cur)?;
    let mut source_sha256 = [0u8; 32];
    source_sha256.copy_from_slice(r.take(32, cur)?);
    let argc = r.len(cur)?;
    // NO pre-allocation on `argc` — each arg reads ≥ 4 bytes; cap the reserve at
    // remaining so a bomb count is a clean `Truncated`, never an OOM.
    let mut argv = Vec::with_capacity(argc.min(r.remaining()));
    for _ in 0..argc {
        argv.push(r.str(cur)?);
    }
    let test_name = r.opt_str(cur)?;
    let filter = r.opt_str(cur)?;
    let created_ms = r.f64(cur)?;
    let engine = r.u8(cur)?;
    if !r.at_end() {
        // Trailing header bytes — a corrupt or forward-incompatible header.
        return Err(TraceError::Truncated { record: None });
    }
    Ok(TraceHeader {
        seed,
        start_ms,
        kind,
        program_path,
        source_sha256,
        argv,
        test_name,
        filter,
        created_ms,
        engine,
    })
}

// ===========================================================================
// Top-level encode / decode
// ===========================================================================

/// Serialize a full trace to a byte vector (magic → version → header → records
/// → end marker). The public [`write_trace`] adds the atomic temp+rename.
fn encode_trace(header: &TraceHeader, events: &[DetEvent]) -> Vec<u8> {
    let mut out = Writer::new();
    out.buf.extend_from_slice(&TRACE_MAGIC);
    out.u16(TRACE_FORMAT_VERSION);

    let body = encode_header_body(header);
    out.u32(u32::try_from(body.len()).unwrap_or(u32::MAX));
    out.buf.extend_from_slice(&body);
    out.u32(crc32(&body));

    for ev in events {
        let (kind, payload) = encode_event(ev);
        out.u8(kind);
        out.u32(u32::try_from(payload.len()).unwrap_or(u32::MAX));
        out.buf.extend_from_slice(&payload);
        // crc32 over kind ‖ len ‖ payload.
        let mut crc_in = Vec::with_capacity(5 + payload.len());
        crc_in.push(kind);
        crc_in.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        crc_in.extend_from_slice(&payload);
        out.u32(crc32(&crc_in));
    }

    out.u8(0xFF);
    out.u32(u32::try_from(events.len()).unwrap_or(u32::MAX));
    out.buf
}

/// Decode + verify a full `ASTRC` trace from `bytes`. HOSTILE-SAFE: every length
/// is bounds-checked against the live buffer, every record's crc32 is verified,
/// an unknown version/record-kind or a truncation at ANY offset is a clean
/// [`TraceError`] naming the failing record. NEVER panics, NEVER allocates on an
/// unverified declared length.
pub fn read_trace(bytes: &[u8]) -> Result<(TraceHeader, Vec<DetEvent>), TraceError> {
    let mut r = Reader::new(bytes);

    // Magic.
    let magic = r.take(8, None)?;
    if magic != TRACE_MAGIC {
        return Err(TraceError::BadMagic);
    }

    // Version — reject a stream newer than we understand.
    let version = r.u16(None)?;
    if version > TRACE_FORMAT_VERSION {
        return Err(TraceError::VersionTooNew {
            found: version,
            max: TRACE_FORMAT_VERSION,
        });
    }

    // Header — length-prefixed body + crc.
    let header_len = r.len(None)?;
    let body = r.take(header_len, None)?;
    let header_crc = r.u32(None)?;
    if crc32(body) != header_crc {
        return Err(TraceError::BadCrc { record: None });
    }
    let header = decode_header_body(body)?;

    // Records until the end marker.
    let mut events: Vec<DetEvent> = Vec::new();
    loop {
        let cur = events.len();
        let tag = r.u8(Some(cur))?;
        if tag == 0xFF {
            // End marker — verify the stored count matches and nothing trails.
            let count = r.u32(None)?;
            if count as usize != events.len() {
                return Err(TraceError::BadEndMarker);
            }
            if !r.at_end() {
                return Err(TraceError::TrailingBytes);
            }
            break;
        }
        // A record: tag is the kind; read payload_len + payload + crc.
        let payload_len = r.len(Some(cur))?;
        let payload = r.take(payload_len, Some(cur))?;
        let rec_crc = r.u32(Some(cur))?;
        // crc over kind ‖ len ‖ payload.
        let mut crc_in = Vec::with_capacity(5 + payload.len());
        crc_in.push(tag);
        crc_in.extend_from_slice(&(payload_len as u32).to_le_bytes());
        crc_in.extend_from_slice(payload);
        if crc32(&crc_in) != rec_crc {
            return Err(TraceError::BadCrc { record: Some(cur) });
        }
        // Decode the payload against a reader bounded to exactly the payload.
        let mut pr = Reader::new(payload);
        let ev = decode_event(tag, &mut pr, cur)?;
        events.push(ev);
    }

    Ok((header, events))
}

/// Write a full trace to `path` atomically (temp file + fsync + rename — the
/// `workflow::write_log` pattern). The temp file is removed on a failed commit.
// WASM §5.3: on wasm `std::fs::File` is a stub whose `Drop` is a no-op, so the
// explicit `drop(f)` (a meaningful early fd-close-before-rename on native, esp.
// Windows) trips `clippy::drop_non_drop`. Native behavior + lint are unchanged.
#[cfg_attr(target_family = "wasm", allow(clippy::drop_non_drop))]
pub fn write_trace(path: &Path, header: &TraceHeader, events: &[DetEvent]) -> Result<(), AsError> {
    use std::io::Write;
    let bytes = encode_trace(header, events);

    let tmp = {
        let mut t = path.as_os_str().to_owned();
        t.push(format!(".{}.tmp", std::process::id()));
        std::path::PathBuf::from(t)
    };

    let mut f = std::fs::File::create(&tmp).map_err(|e| {
        AsError::new(format!("trace: cannot write '{}': {}", tmp.display(), e))
    })?;
    f.write_all(&bytes)
        .map_err(|e| AsError::new(format!("trace: write failed: {}", e)))?;
    // A trace is the artifact you replay after a crash — make it durable.
    f.sync_all()
        .map_err(|e| AsError::new(format!("trace: sync failed: {}", e)))?;
    drop(f);

    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AsError::new(format!("trace: commit failed: {}", e))
    })?;

    // Fsync the parent directory so the rename is itself durable (best-effort).
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header_run() -> TraceHeader {
        TraceHeader {
            seed: 0xDEAD_BEEF_CAFE_F00D,
            start_ms: 1_718_000_000_123.5,
            kind: TraceKind::Run,
            program_path: "examples/api.as".to_string(),
            source_sha256: [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23, 24, 25, 26, 27, 28, 29, 30, 31,
            ],
            argv: vec!["--port".to_string(), "8080".to_string()],
            test_name: None,
            filter: None,
            created_ms: 1_718_000_000_999.0,
            engine: 7,
        }
    }

    fn sample_header_test() -> TraceHeader {
        TraceHeader {
            seed: 42,
            start_ms: f64::from_bits(0x4010_0000_0000_0001), // a non-trivial bit pattern
            kind: TraceKind::Test,
            program_path: "tests/mytest.as".to_string(),
            source_sha256: [0xAB; 32],
            argv: vec![],
            test_name: Some("handles retries".to_string()),
            filter: Some("retry".to_string()),
            created_ms: -0.0,
            engine: 0,
        }
    }

    /// Every `DetEvent` variant, every `TraceOutcome`, every `FfiRet`.
    fn all_events() -> Vec<DetEvent> {
        vec![
            DetEvent::ClockRead { value: 1.5 },
            DetEvent::RandomRead {
                value: f64::from_bits(0x3FF0_0000_0000_0001),
            },
            DetEvent::BytesRead {
                bytes: vec![0, 255, 1, 254, 2, 253, 42],
            },
            DetEvent::MonotonicRead { value: 999.25 },
            DetEvent::TimerSet { wake: 5000.0 },
            DetEvent::ActivityCompleted {
                name: "charge".to_string(),
                args_hash: 0x1122_3344_5566_7788,
                result_json: r#"{"ok":true}"#.to_string(),
            },
            DetEvent::ActorCall {
                method: "tick".to_string(),
                result: vec![9, 8, 7],
                panic: None,
            },
            DetEvent::ActorCall {
                method: "boom".to_string(),
                result: vec![],
                panic: Some("actor exploded".to_string()),
            },
            DetEvent::FfiCall {
                ret: FfiRet::Int(-12345),
                out_params: vec![(0, vec![1, 2, 3]), (3, vec![255])],
            },
            DetEvent::FfiCall {
                ret: FfiRet::Float(3.25),
                out_params: vec![],
            },
            DetEvent::FfiCall {
                ret: FfiRet::Void,
                out_params: vec![],
            },
            DetEvent::GeneratorYield {
                value: Some(vec![10, 20, 30]),
                panic: None,
            },
            DetEvent::GeneratorYield {
                value: None,
                panic: None,
            },
            DetEvent::GeneratorYield {
                value: None,
                panic: Some("producer failed".to_string()),
            },
            DetEvent::StdlibCall {
                module: "http".to_string(),
                func: "get".to_string(),
                args_hash: 0xAAAA_BBBB_CCCC_DDDD,
                outcome: TraceOutcome::Value(vec![1, 2, 3, 4]),
            },
            DetEvent::StdlibCall {
                module: "http".to_string(),
                func: "post".to_string(),
                args_hash: 1,
                outcome: TraceOutcome::Panic("network down".to_string()),
            },
            DetEvent::StdlibCall {
                module: "fs".to_string(),
                func: "read".to_string(),
                args_hash: 2,
                outcome: TraceOutcome::Propagate(vec![0xFF, 0x00]),
            },
            DetEvent::NativeCall {
                vid: 7,
                method: "json".to_string(),
                args_hash: 3,
                outcome: TraceOutcome::Value(vec![]),
            },
            DetEvent::NativeCall {
                vid: 4_000_000_000,
                method: "text".to_string(),
                args_hash: 4,
                outcome: TraceOutcome::Panic("boom".to_string()),
            },
            DetEvent::NativeCall {
                vid: 0,
                method: "bytes".to_string(),
                args_hash: 5,
                outcome: TraceOutcome::Propagate(vec![7]),
            },
            // REPLAY §2.5 — a virtualized HttpResponse handle-birth outcome.
            DetEvent::StdlibCall {
                module: "net_http".to_string(),
                func: "get".to_string(),
                args_hash: 0xDEAD_BEEF,
                outcome: TraceOutcome::Handle {
                    kind_tag: 1,
                    vid: 0,
                    fields: vec![0x10, 0x20, 0x30, 0x40, 0x50],
                },
            },
            DetEvent::StdlibCall {
                module: "net_http".to_string(),
                func: "post".to_string(),
                args_hash: 0,
                outcome: TraceOutcome::Handle {
                    kind_tag: 1,
                    vid: 4_294_967_295,
                    fields: vec![],
                },
            },
        ]
    }

    #[test]
    fn crc32_canonical_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }

    #[test]
    fn header_run_roundtrips() {
        let h = sample_header_run();
        let bytes = encode_trace(&h, &[]);
        let (got, events) = read_trace(&bytes).expect("decode");
        assert_eq!(got, h);
        assert!(events.is_empty());
        // f64 bit-identity.
        assert_eq!(got.start_ms.to_bits(), h.start_ms.to_bits());
        assert_eq!(got.created_ms.to_bits(), h.created_ms.to_bits());
    }

    #[test]
    fn header_test_roundtrips() {
        let h = sample_header_test();
        let bytes = encode_trace(&h, &[]);
        let (got, events) = read_trace(&bytes).expect("decode");
        assert_eq!(got, h);
        assert!(events.is_empty());
        assert_eq!(got.test_name.as_deref(), Some("handles retries"));
        assert_eq!(got.filter.as_deref(), Some("retry"));
        // -0.0 bit-identity (not just == which folds -0.0 == 0.0).
        assert_eq!(got.created_ms.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn full_event_stream_roundtrips() {
        let h = sample_header_run();
        let events = all_events();
        let bytes = encode_trace(&h, &events);
        let (got_h, got_events) = read_trace(&bytes).expect("decode");
        assert_eq!(got_h, h);
        assert_eq!(got_events, events);
    }

    #[test]
    fn truncation_at_every_offset_is_clean_err() {
        let h = sample_header_test();
        let events = all_events();
        let full = encode_trace(&h, &events);
        // Every strict prefix must be a clean Err — never a panic, never Ok.
        for n in 0..full.len() {
            let res = std::panic::catch_unwind(|| read_trace(&full[..n]));
            match res {
                Ok(Ok(_)) => panic!("prefix of len {n} decoded Ok — should be truncated"),
                Ok(Err(_)) => {} // clean error — good
                Err(_) => panic!("prefix of len {n} PANICKED — reader is not hostile-safe"),
            }
        }
        // The full buffer still decodes.
        assert!(read_trace(&full).is_ok());
    }

    #[test]
    fn empty_and_magic_only_are_err() {
        assert_eq!(read_trace(&[]).unwrap_err(), TraceError::Truncated { record: None });
        assert!(read_trace(&TRACE_MAGIC).is_err());
        // Magic + partial version.
        let mut b = TRACE_MAGIC.to_vec();
        b.push(1);
        assert!(read_trace(&b).is_err());
    }

    #[test]
    fn bad_magic_is_err() {
        let mut bytes = encode_trace(&sample_header_run(), &all_events());
        bytes[0] ^= 0xFF;
        assert_eq!(read_trace(&bytes).unwrap_err(), TraceError::BadMagic);
    }

    #[test]
    fn unknown_version_is_newer_err() {
        let mut bytes = encode_trace(&sample_header_run(), &[]);
        // version is the two bytes after the 8-byte magic.
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        match read_trace(&bytes) {
            Err(TraceError::VersionTooNew { found: 2, max: 1 }) => {}
            other => panic!("expected VersionTooNew, got {other:?}"),
        }
        // The Display string is the documented one.
        let msg = read_trace(&bytes).unwrap_err().to_string();
        assert!(msg.contains("newer than this binary supports"), "{msg}");
    }

    #[test]
    fn flipped_header_crc_is_err() {
        let h = sample_header_run();
        let body = encode_header_body(&h);
        let bytes = encode_trace(&h, &[]);
        // header_crc sits at: 8 (magic) + 2 (version) + 4 (header_len) + body.len().
        let crc_off = 8 + 2 + 4 + body.len();
        let mut corrupt = bytes.clone();
        corrupt[crc_off] ^= 0x01;
        assert_eq!(
            read_trace(&corrupt).unwrap_err(),
            TraceError::BadCrc { record: None }
        );
    }

    #[test]
    fn flipped_record_crc_is_err() {
        let h = sample_header_run();
        let events = vec![DetEvent::ClockRead { value: 1.0 }];
        let body = encode_header_body(&h);
        let bytes = encode_trace(&h, &events);
        // First record starts after: magic(8)+ver(2)+hlen(4)+body+hcrc(4).
        let rec_start = 8 + 2 + 4 + body.len() + 4;
        // record = kind(1) + payload_len(4) + payload(8 for f64) + crc(4).
        let rec_crc_off = rec_start + 1 + 4 + 8;
        let mut corrupt = bytes.clone();
        corrupt[rec_crc_off] ^= 0x80;
        assert_eq!(
            read_trace(&corrupt).unwrap_err(),
            TraceError::BadCrc { record: Some(0) }
        );
    }

    #[test]
    fn unknown_record_kind_is_err() {
        // Hand-craft: valid header, then a record with kind=200 (unknown), then
        // a valid crc over it (so we hit UnknownRecordKind, not BadCrc).
        let h = sample_header_run();
        let body = encode_header_body(&h);
        let mut out = Vec::new();
        out.extend_from_slice(&TRACE_MAGIC);
        out.extend_from_slice(&TRACE_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        // Record: kind=200, empty payload, correct crc.
        let kind = 200u8;
        out.push(kind);
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut crc_in = vec![kind];
        crc_in.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&crc32(&crc_in).to_le_bytes());
        // End marker (count would be wrong but we never reach it).
        out.push(0xFF);
        out.extend_from_slice(&1u32.to_le_bytes());
        match read_trace(&out) {
            Err(TraceError::UnknownRecordKind { record: 0, kind: 200 }) => {}
            other => panic!("expected UnknownRecordKind, got {other:?}"),
        }
    }

    #[test]
    fn length_bomb_does_not_oom() {
        // A record whose declared payload_len = u32::MAX over a tiny buffer.
        let h = sample_header_run();
        let body = encode_header_body(&h);
        let mut out = Vec::new();
        out.extend_from_slice(&TRACE_MAGIC);
        out.extend_from_slice(&TRACE_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        // kind=1 (ClockRead), payload_len = u32::MAX, then nothing.
        out.push(REC_CLOCK_READ);
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        // The reader must `take(u32::MAX)` against a ~10-byte remaining buffer →
        // clean Truncated, no allocation.
        match read_trace(&out) {
            Err(TraceError::Truncated { record: Some(0) }) => {}
            other => panic!("expected Truncated at record 0, got {other:?}"),
        }
    }

    #[test]
    fn header_argv_count_bomb_does_not_oom() {
        // A header declaring a huge argv count over a tiny body → clean Err.
        let mut w = Writer::new();
        w.u64(1); // seed
        w.f64(0.0); // start_ms
        w.u8(0); // kind = run
        w.str("p"); // program path
        w.buf.extend_from_slice(&[0u8; 32]); // sha
        w.u32(u32::MAX); // argv count BOMB
        // ... no args follow.
        let body = w.buf;
        let mut out = Vec::new();
        out.extend_from_slice(&TRACE_MAGIC);
        out.extend_from_slice(&TRACE_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        out.push(0xFF);
        out.extend_from_slice(&0u32.to_le_bytes());
        // The header crc passes; decode_header_body hits the argv bomb → Truncated.
        assert!(read_trace(&out).is_err());
    }

    #[test]
    fn ffi_out_params_count_bomb_does_not_oom() {
        // A valid FfiCall record header but a bomb out_params count.
        let mut w = Writer::new();
        w.u8(FFI_VOID);
        w.u32(u32::MAX); // out_params count BOMB
        let payload = w.buf;
        let h = sample_header_run();
        let body = encode_header_body(&h);
        let mut out = Vec::new();
        out.extend_from_slice(&TRACE_MAGIC);
        out.extend_from_slice(&TRACE_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        out.push(REC_FFI_CALL);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        let mut crc_in = vec![REC_FFI_CALL];
        crc_in.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        crc_in.extend_from_slice(&payload);
        out.extend_from_slice(&crc32(&crc_in).to_le_bytes());
        out.push(0xFF);
        out.extend_from_slice(&1u32.to_le_bytes());
        // The payload reader is bounded to the FfiCall payload; the bomb count
        // exceeds it → clean Truncated, no OOM.
        match read_trace(&out) {
            Err(TraceError::Truncated { record: Some(0) }) => {}
            other => panic!("expected Truncated at record 0, got {other:?}"),
        }
    }

    #[test]
    fn wrong_event_count_is_err() {
        let h = sample_header_run();
        let events = vec![DetEvent::ClockRead { value: 1.0 }];
        let mut bytes = encode_trace(&h, &events);
        // The last 4 bytes are the event_count; corrupt it.
        let n = bytes.len();
        bytes[n - 4..].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(read_trace(&bytes).unwrap_err(), TraceError::BadEndMarker);
    }

    #[test]
    fn trailing_bytes_after_end_marker_is_err() {
        let h = sample_header_run();
        let mut bytes = encode_trace(&h, &[]);
        bytes.push(0xAB);
        assert_eq!(read_trace(&bytes).unwrap_err(), TraceError::TrailingBytes);
    }

    #[test]
    fn write_trace_is_atomic_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!(
            "ascript_trace_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.astrc");

        let h = sample_header_test();
        let events = all_events();
        write_trace(&path, &h, &events).expect("write");

        // The file exists and round-trips.
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        let (got_h, got_events) = read_trace(&bytes).expect("decode written");
        assert_eq!(got_h, h);
        assert_eq!(got_events, events);

        // No stray `.tmp` sibling survived the successful commit.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
