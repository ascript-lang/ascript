//! `std/archive` — streaming tar (BATT B1 §6).
//!
//! The streaming superset of `std/compress`'s one-shot `tarCreate`/`tarExtract`.
//! Two surfaces:
//!
//!   - a **writer handle** (`tarWriter(opts?)` → `Value::native(ArchiveWriter)`):
//!     `add(name, data, opts?)` appends one entry incrementally and `finish()`
//!     consumes the handle and returns the assembled bytes (gzip-wrapped if
//!     `{gzip:true}`; byte-deterministic if `{deterministic:true}`).
//!   - a **lazy entries generator** (`tarEntries(bytes)` → `Value::generator`):
//!     each `next()` decodes ONE entry header + data and yields a
//!     `{name, size, mode, isDir, data}` object. Decoding is done up-front into a
//!     bounded in-memory list (each entry's data is read incrementally, capped at
//!     `MAX_ENTRY_BYTES` so a hostile declared size can never `Vec::with_capacity`
//!     a 4 GiB buffer or OOM the host); a corrupt/truncated header surfaces as a
//!     Tier-1 `[nil, err]` pair on the `next()` that reaches it, AFTER the prior
//!     entries have yielded fine (the generator-protocol laziness guarantee).
//!
//! Hostile tar input is the security focus: every allocation is bounded, a
//! truncated/garbage stream yields a clean Tier-1 result, and nothing in the
//! decode path can panic or abort.
//!
//! The disk-touching fns (`tarExtractTo`/`zipExtractTo`/`tarCreateFromDir`) land
//! in B2 — the `required_cap("archive", …)` `Fs` arm is wired ahead of them.

use super::{arg, bi};
use crate::coro::{current_generator, GeneratorHandle};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

/// Hard upper bound on any single tar entry's data we will buffer in memory: 256
/// MiB. A tar header carries a 12-octal-digit size field (up to ~64 GiB), and a
/// GNU/PAX extension can declare even larger; an attacker setting it to
/// `0xFFFFFFFFFFF` must NOT cause a `Vec::with_capacity(huge)` or an unbounded
/// read. We read incrementally with a small fixed buffer and stop (Tier-1) the
/// moment an entry exceeds this cap. Legitimate archives stay well under it.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

// ── plain-Rust core (spec §6.6 — no Value types) ─────────────────────────────

/// The in-memory tar builder behind a [`NativeKind::ArchiveWriter`] handle. Pure
/// Rust over the vendored `tar`/`flate2` crates — no `Value` types, so it is unit-
/// testable in isolation. `gzip` wraps the finished tar; `deterministic` zeroes
/// the mtime/uid/gid of every entry so two identical add-sequences produce
/// byte-identical output.
pub struct TarBuild {
    builder: tar::Builder<Vec<u8>>,
    gzip: bool,
    deterministic: bool,
}

impl TarBuild {
    pub(crate) fn new(gzip: bool, deterministic: bool) -> Self {
        TarBuild {
            builder: tar::Builder::new(Vec::new()),
            gzip,
            deterministic,
        }
    }

    /// Append one entry. `dir=true` writes a directory header (empty data,
    /// `entry_type=Directory`, mode defaulting to `0o755`); otherwise a regular
    /// file with `data`. A failure (e.g. an over-long path the tar writer rejects)
    /// is a `String` the caller surfaces however it likes.
    pub(crate) fn add(
        &mut self,
        name: &str,
        data: &[u8],
        mode: u32,
        mtime: u64,
        dir: bool,
    ) -> Result<(), String> {
        let mut header = tar::Header::new_gnu();
        let (size, etype) = if dir {
            (0u64, tar::EntryType::Directory)
        } else {
            (data.len() as u64, tar::EntryType::Regular)
        };
        header.set_size(size);
        header.set_mode(mode);
        // Deterministic builds zero the volatile metadata so the bytes are stable.
        if self.deterministic {
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
        } else {
            header.set_mtime(mtime);
        }
        header.set_entry_type(etype);
        header.set_cksum();
        let body: &[u8] = if dir { &[] } else { data };
        self.builder
            .append_data(&mut header, name, body)
            .map_err(|e| format!("archive add failed: {}", e))
    }

    /// Finalize: flush the tar footer, then gzip-wrap if requested. Consumes the
    /// builder.
    pub(crate) fn finish(self) -> Result<Vec<u8>, String> {
        let TarBuild {
            builder, gzip, ..
        } = self;
        let tar_bytes = builder
            .into_inner()
            .map_err(|e| format!("archive finish failed: {}", e))?;
        if !gzip {
            return Ok(tar_bytes);
        }
        use std::io::Write;
        let mut enc =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tar_bytes)
            .and_then(|_| enc.finish())
            .map_err(|e| format!("archive gzip failed: {}", e))
    }
}

/// The `ResourceState` payload (interp.rs `ResourceState::ArchiveWriter`). An enum
/// so B2's zip writer can join without a new `ResourceState` variant.
pub enum ArchiveWriterState {
    Tar(TarBuild),
}

// ── decode core (hostile-safe) ───────────────────────────────────────────────

/// One decoded tar entry, before it becomes a `Value`.
struct DecodedEntry {
    name: String,
    size: u64,
    mode: u32,
    is_dir: bool,
    data: Vec<u8>,
}

/// Magic-sniff gzip (`1f 8b`) and decompress if present, else return the bytes
/// unchanged. Decompression is bounded — a gzip bomb cannot expand past
/// `MAX_ENTRY_BYTES * 64` (a generous archive-total ceiling) before we stop.
fn maybe_gunzip(raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut dec = flate2::read::GzDecoder::new(raw);
        let mut out = Vec::new();
        // Bound the inflate so a gzip bomb cannot OOM: read in chunks, cap total.
        let cap = MAX_ENTRY_BYTES.saturating_mul(64);
        if let Err(e) = read_to_end_bounded(&mut dec, &mut out, cap) {
            return Err(format!("archive gunzip failed: {}", e));
        }
        Ok(out)
    } else {
        Ok(raw.to_vec())
    }
}

/// Read `r` to EOF into `out`, but never let `out` exceed `cap` bytes. Uses a
/// fixed 64 KiB scratch buffer and grows `out` only as data actually arrives —
/// so a hostile declared length never pre-reserves a giant `Vec`. Returns an
/// error string if the cap is exceeded.
fn read_to_end_bounded<R: Read>(r: &mut R, out: &mut Vec<u8>, cap: u64) -> Result<(), String> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if out.len() as u64 + n as u64 > cap {
            return Err(format!("entry exceeds {} byte cap", cap));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(())
}

/// Decode every entry of a (possibly gzipped) tar into a bounded in-memory list.
/// A corrupt/truncated header stops decoding and is returned as the trailing
/// `Err` PAIRED with the entries decoded so far — the caller (the generator)
/// yields the good entries first, then surfaces the error on the next pull.
///
/// Every allocation is bounded: an entry's declared size is consulted only as a
/// pre-check against `MAX_ENTRY_BYTES` (a hostile `0xFFFFFFFFFFF` → immediate
/// Tier-1, no allocation), and the actual data read is incremental + capped, so
/// a header that LIES about its size (small declared, infinite stream) also stops
/// at the cap rather than reading forever.
fn decode_tar(bytes: &[u8]) -> (Vec<DecodedEntry>, Option<String>) {
    let raw = match maybe_gunzip(bytes) {
        Ok(r) => r,
        Err(e) => return (Vec::new(), Some(e)),
    };
    let mut out = Vec::new();
    let mut archive = tar::Archive::new(std::io::Cursor::new(raw));
    let entries = match archive.entries() {
        Ok(it) => it,
        Err(e) => return (out, Some(format!("archive read failed: {}", e))),
    };
    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => return (out, Some(format!("archive entry header failed: {}", e))),
        };
        // Declared size is the FIRST hostile-input gate: reject a giant size
        // BEFORE touching any buffer (no `Vec::with_capacity(declared)`).
        let declared = entry.header().size().unwrap_or(0);
        if declared > MAX_ENTRY_BYTES {
            return (
                out,
                Some(format!(
                    "archive entry size {} exceeds {} byte cap",
                    declared, MAX_ENTRY_BYTES
                )),
            );
        }
        let name = match entry.path() {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return (out, Some(format!("archive entry path failed: {}", e))),
        };
        let etype = entry.header().entry_type();
        let is_dir = etype.is_dir();
        let mode = entry.header().mode().unwrap_or(0o644);
        // Read the data incrementally, capped — a header that lies (small
        // declared, endless stream) stops at the cap, not at OOM.
        let mut data = Vec::new();
        if let Err(e) = read_to_end_bounded(&mut entry, &mut data, MAX_ENTRY_BYTES) {
            return (out, Some(format!("archive entry data failed: {}", e)));
        }
        let size = data.len() as u64;
        out.push(DecodedEntry {
            name,
            size,
            mode,
            is_dir,
            data,
        });
    }
    (out, None)
}

// ── Value plumbing ───────────────────────────────────────────────────────────

fn bytes_val(v: Vec<u8>) -> Value {
    Value::bytes_rc(Rc::new(RefCell::new(v)))
}

fn entry_to_value(e: DecodedEntry) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("name".to_string(), Value::str(e.name));
    m.insert("size".to_string(), Value::int(e.size as i64));
    m.insert("mode".to_string(), Value::int(e.mode as i64));
    m.insert("isDir".to_string(), Value::bool_(e.is_dir));
    m.insert("data".to_string(), bytes_val(e.data));
    Value::object(m)
}

/// Accept bytes (or a UTF-8 string) as a source of raw archive bytes.
fn source_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v.kind() {
        ValueKind::Bytes(b) => Ok(b.borrow().clone()),
        ValueKind::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(AsError::at(
            format!("{} expects bytes, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

/// Read a boolean field from an options object (missing/non-object → `false`).
fn opt_bool(opts: &Value, key: &str) -> bool {
    if let ValueKind::Object(o) = opts.kind() {
        if let Some(v) = o.get(key) {
            return v.is_truthy();
        }
    }
    false
}

/// Read an optional integer field from an options object.
fn opt_int(opts: &Value, key: &str) -> Option<i64> {
    if let ValueKind::Object(o) = opts.kind() {
        if let Some(v) = o.get(key) {
            return v.as_int_exact();
        }
    }
    None
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("tarWriter", bi("archive.tarWriter")),
        ("tarEntries", bi("archive.tarEntries")),
        ("tarAppend", bi("archive.tarAppend")),
    ]
}

/// Qualified dispatch for `archive.*`. Needs `&Interp` to register the writer
/// resource handle and to build the entries generator.
pub fn call(interp: &Interp, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "tarWriter" => {
            let opts = arg(args, 0);
            let gzip = opt_bool(&opts, "gzip");
            let deterministic = opt_bool(&opts, "deterministic");
            let state = ResourceState::ArchiveWriter(Box::new(ArchiveWriterState::Tar(
                TarBuild::new(gzip, deterministic),
            )));
            Ok(interp.register_resource(NativeKind::ArchiveWriter, indexmap::IndexMap::new(), state))
        }
        "tarEntries" => {
            let raw = source_bytes(&arg(args, 0), span, "archive.tarEntries")?;
            Ok(make_entries_generator(raw))
        }
        "tarAppend" => tar_append(interp, args, span),
        _ => Err(AsError::at(format!("unknown archive function '{}'", func), span).into()),
    }
}

/// `tarAppend(bytes, additions)` — decode `bytes` (preserving originals), then
/// append each `{name, data, mode?, dir?}` of `additions`, and return the new
/// archive bytes (Tier-1: a corrupt source archive → `[nil, err]`).
fn tar_append(_interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let raw = source_bytes(&arg(args, 0), span, "archive.tarAppend")?;
    let (originals, derr) = decode_tar(&raw);
    if let Some(e) = derr {
        return Ok(make_pair(Value::nil(), make_error(Value::str(e))));
    }
    let additions = arg(args, 1);
    let add_list = match additions.kind() {
        ValueKind::Array(a) => a.borrow().clone(),
        ValueKind::Nil => Vec::new(),
        _ => {
            return Err(AsError::at(
                format!(
                    "archive.tarAppend additions must be an array, got {}",
                    crate::interp::type_name(&additions)
                ),
                span,
            )
            .into())
        }
    };
    // The append preserves the source's deterministic-free metadata is irrelevant
    // here; rebuild from a non-gzip, non-deterministic builder (tarAppend returns
    // raw tar bytes; gzip/determinism is a writer concern).
    let mut build = TarBuild::new(false, false);
    for e in originals {
        if let Err(msg) = build.add(&e.name, &e.data, e.mode, 0, e.is_dir) {
            return Ok(make_pair(Value::nil(), make_error(Value::str(msg))));
        }
    }
    for entry in &add_list {
        let obj = match entry.kind() {
            ValueKind::Object(o) => o.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "archive.tarAppend entry must be an object, got {}",
                        crate::interp::type_name(entry)
                    ),
                    span,
                )
                .into())
            }
        };
        let name = match obj.get("name").as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => s.to_string(),
            _ => {
                return Err(AsError::at(
                    "archive.tarAppend entry.name must be a string".to_string(),
                    span,
                )
                .into())
            }
        };
        let dir = obj.get("dir").map(|v| v.is_truthy()).unwrap_or(false);
        let mode = obj
            .get("mode")
            .and_then(|v| v.as_int_exact())
            .map(|m| m as u32)
            .unwrap_or(if dir { 0o755 } else { 0o644 });
        let data: Vec<u8> = match obj.get("data").as_ref().map(|v| v.kind()) {
            Some(ValueKind::Bytes(b)) => b.borrow().clone(),
            Some(ValueKind::Str(s)) => s.as_bytes().to_vec(),
            Some(ValueKind::Nil) | None => Vec::new(),
            Some(_) => {
                return Err(AsError::at(
                    "archive.tarAppend entry.data must be bytes or a string".to_string(),
                    span,
                )
                .into())
            }
        };
        if let Err(msg) = build.add(&name, &data, mode, 0, dir) {
            return Ok(make_pair(Value::nil(), make_error(Value::str(msg))));
        }
    }
    match build.finish() {
        Ok(b) => Ok(make_pair(bytes_val(b), Value::nil())),
        Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
    }
}

/// Build the lazy `tarEntries` generator. The body decodes the archive up-front
/// into a bounded list (decode is the security-critical part — every allocation
/// is bounded), then yields one entry per `next()`. A decode error is yielded as
/// a final `[nil, err]` Tier-1 pair AFTER the good entries, so prior entries pull
/// fine and the corrupt entry surfaces on the `next()` that reaches it.
///
/// THE NOVEL BIT — a native in-memory generator: the body is a plain Rust
/// `async move` future that drives [`GeneratorHandle::yield_`] via
/// `current_generator()` (the exact path the `yield` expression uses at runtime),
/// mirroring `coro::tests::make_gen`. No script body, no VM fiber, no worker
/// isolate — just an in-memory cursor handed to `GeneratorHandle::new`.
fn make_entries_generator(raw: Vec<u8>) -> Value {
    let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
        Box::pin(async move {
            let (entries, decode_err) = decode_tar(&raw);
            for e in entries {
                let g = current_generator().expect("inside a generator");
                g.yield_(entry_to_value(e)).await;
            }
            // A corrupt/truncated stream → surface a final Tier-1 pair, THEN finish.
            if let Some(msg) = decode_err {
                let g = current_generator().expect("inside a generator");
                g.yield_(make_pair(Value::nil(), make_error(Value::str(msg))))
                    .await;
            }
            Ok(Value::nil())
        });
    Value::generator(Rc::new(GeneratorHandle::new(body)))
}

// ── writer handle methods (interp.rs call_native_method routes here) ──────────

/// `writer.add(name, data, opts?)` / `writer.finish()`. Synchronous (in-memory),
/// so no take-out-across-await dance is needed — we borrow the resource, mutate,
/// and (for `finish`) consume it via `take_resource`.
pub fn call_writer_method(
    interp: &Interp,
    m: &NativeMethod,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    let id = m.receiver.id;
    match m.method.as_str() {
        "add" => {
            let name_v = arg(args, 0);
            let name = match name_v.kind() {
                ValueKind::Str(s) => s.to_string(),
                _ => {
                    return Err(AsError::at(
                        format!(
                            "archiveWriter.add name must be a string, got {}",
                            crate::interp::type_name(&name_v)
                        ),
                        span,
                    )
                    .into())
                }
            };
            let opts = arg(args, 2);
            let dir = opt_bool(&opts, "dir");
            let mode = opt_int(&opts, "mode")
                .map(|m| m as u32)
                .unwrap_or(if dir { 0o755 } else { 0o644 });
            let mtime = opt_int(&opts, "mtime").map(|t| t as u64).unwrap_or(0);
            let data_v = arg(args, 1);
            let data: Vec<u8> = match data_v.kind() {
                ValueKind::Bytes(b) => b.borrow().clone(),
                ValueKind::Str(s) => s.as_bytes().to_vec(),
                ValueKind::Nil => Vec::new(),
                _ => {
                    return Err(AsError::at(
                        format!(
                            "archiveWriter.add data must be bytes, a string, or nil, got {}",
                            crate::interp::type_name(&data_v)
                        ),
                        span,
                    )
                    .into())
                }
            };
            // Take the resource out, append, put it back (in-memory, no await; the
            // take/return keeps the `resources` borrow off any nested call). A
            // missing/closed handle → Tier-2 (used after finish()).
            let mut state = match interp.take_resource(id) {
                Some(ResourceState::ArchiveWriter(s)) => s,
                other => {
                    if let Some(o) = other {
                        interp.return_resource(id, o);
                    }
                    return Err(AsError::at(
                        "archiveWriter has already been finished".to_string(),
                        span,
                    )
                    .into());
                }
            };
            let ArchiveWriterState::Tar(build) = state.as_mut();
            let res = build.add(&name, &data, mode, mtime, dir);
            interp.return_resource(id, ResourceState::ArchiveWriter(state));
            match res {
                Ok(()) => Ok(Value::nil()),
                Err(msg) => Err(AsError::at(msg, span).into()),
            }
        }
        "finish" => {
            // `finish` CONSUMES the builder: take the resource out, finalize, and
            // leave the handle Consumed so a later use is a clean Tier-2.
            let state = match interp.take_resource(id) {
                Some(ResourceState::ArchiveWriter(s)) => s,
                other => {
                    if let Some(o) = other {
                        interp.return_resource(id, o);
                    }
                    return Err(AsError::at(
                        "archiveWriter has already been finished".to_string(),
                        span,
                    )
                    .into());
                }
            };
            let ArchiveWriterState::Tar(build) = *state;
            match build.finish() {
                Ok(b) => Ok(bytes_val(b)),
                Err(msg) => Err(AsError::at(msg, span).into()),
            }
        }
        other => Err(AsError::at(
            format!("unknown archiveWriter method '{}'", other),
            span,
        )
        .into()),
    }
}

#[cfg(all(test, feature = "archive"))]
mod tests {
    use super::*;
    use crate::stdlib::MAX_ALLOC_COUNT;

    // ── plain-Rust core round-trips (no Value/Interp needed) ─────────────────

    /// (a) writer → finish → decode round-trips names/sizes/modes/data + a dir.
    #[test]
    fn writer_roundtrip_names_sizes_modes_data() {
        let mut b = TarBuild::new(false, false);
        b.add("a.txt", b"hello", 0o644, 0, false).unwrap();
        b.add("dir/", &[], 0o755, 0, true).unwrap();
        b.add("b.bin", &[1, 2, 3, 4], 0o600, 0, false).unwrap();
        let bytes = b.finish().unwrap();

        let (entries, err) = decode_tar(&bytes);
        assert!(err.is_none(), "clean archive must decode without error");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 5);
        assert_eq!(entries[0].mode & 0o777, 0o644);
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].data, b"hello");
        assert!(entries[1].name.starts_with("dir"));
        assert!(entries[1].is_dir);
        assert_eq!(entries[1].size, 0);
        assert_eq!(entries[2].name, "b.bin");
        assert_eq!(entries[2].data, vec![1, 2, 3, 4]);
        assert_eq!(entries[2].mode & 0o777, 0o600);
    }

    /// (b) gzip writer output is gzip-wrapped; decode magic-sniffs it.
    #[test]
    fn gzip_writer_is_sniffed_on_decode() {
        let mut b = TarBuild::new(true, false);
        b.add("g.txt", b"gzipped", 0o644, 0, false).unwrap();
        let bytes = b.finish().unwrap();
        // gzip magic.
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "output must be gzip-wrapped");

        let (entries, err) = decode_tar(&bytes);
        assert!(err.is_none());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "g.txt");
        assert_eq!(entries[0].data, b"gzipped");
    }

    /// (c) deterministic: two identical add-sequences → byte-identical output.
    #[test]
    fn deterministic_output_is_byte_identical() {
        let build = || {
            let mut b = TarBuild::new(false, true);
            b.add("x.txt", b"same", 0o644, 12345, false).unwrap();
            b.add("y/", &[], 0o755, 67890, true).unwrap();
            b.finish().unwrap()
        };
        let a = build();
        let c = build();
        assert_eq!(a, c, "deterministic builds must be byte-identical");

        // And a NON-deterministic build with differing mtimes is NOT identical.
        let nd = |t: u64| {
            let mut b = TarBuild::new(false, false);
            b.add("x.txt", b"same", 0o644, t, false).unwrap();
            b.finish().unwrap()
        };
        assert_ne!(nd(1000), nd(2000), "non-deterministic mtime must differ");
    }

    /// (d) tarAppend (core-level): originals preserved + additions appended.
    #[test]
    fn append_preserves_and_adds() {
        let mut base = TarBuild::new(false, false);
        base.add("orig.txt", b"original", 0o644, 0, false).unwrap();
        let base_bytes = base.finish().unwrap();

        // Re-decode + rebuild with one extra entry (mirrors tar_append's core).
        let (originals, derr) = decode_tar(&base_bytes);
        assert!(derr.is_none());
        let mut build = TarBuild::new(false, false);
        for e in &originals {
            build.add(&e.name, &e.data, e.mode, 0, e.is_dir).unwrap();
        }
        build.add("added.txt", b"appended", 0o644, 0, false).unwrap();
        let out = build.finish().unwrap();

        let (entries, err) = decode_tar(&out);
        assert!(err.is_none());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "orig.txt");
        assert_eq!(entries[0].data, b"original");
        assert_eq!(entries[1].name, "added.txt");
        assert_eq!(entries[1].data, b"appended");
    }

    /// (e) LAZINESS: a corrupt 2nd entry → entry 1 decodes fine, the error is the
    /// trailing decode_err (the generator yields entry 1, THEN the error pair).
    #[test]
    fn corrupt_second_entry_yields_first_then_errs() {
        let mut b = TarBuild::new(false, false);
        b.add("good.txt", b"fine", 0o644, 0, false).unwrap();
        b.add("bad.txt", b"corruptme", 0o644, 0, false).unwrap();
        let mut bytes = b.finish().unwrap();

        // Corrupt the SECOND entry's header. Each tar block is 512 bytes; the
        // first header is block 0, its data block 1, the second header block 2.
        // Smash the checksum region of block 2 (offset 512*2 + 148..156).
        let second_header = 512 * 2;
        for byte in bytes
            .iter_mut()
            .skip(second_header + 148)
            .take(8)
        {
            *byte = 0xFF;
        }

        let (entries, err) = decode_tar(&bytes);
        // Entry 1 decoded fine BEFORE the error surfaced.
        assert_eq!(entries.len(), 1, "the first entry must decode");
        assert_eq!(entries[0].name, "good.txt");
        assert_eq!(entries[0].data, b"fine");
        assert!(err.is_some(), "the corrupt 2nd header must produce an error");
    }

    /// (f) HOSTILE battery: truncated, giant size field, and non-tar garbage all
    /// produce a clean (entries, Some(err)) result — NEVER a Rust panic / OOM.
    #[test]
    fn hostile_inputs_are_clean_tier1_never_panic() {
        // Truncated: a single header block cut off mid-way.
        let mut b = TarBuild::new(false, false);
        b.add("t.txt", b"data", 0o644, 0, false).unwrap();
        let full = b.finish().unwrap();
        let truncated = &full[..100]; // mid-header
        let (_e, err) = decode_tar(truncated);
        assert!(err.is_some(), "truncated header must error, not panic");

        // Giant declared size: craft a header whose size octal field is huge.
        // 0xFFFFFFFFFFF = 17592186044415. As 11 octal digits that overflows the
        // field; the tar crate may read it as a large size — we must NOT allocate.
        let mut giant = vec![0u8; 512];
        // name
        giant[..5].copy_from_slice(b"big\0\0");
        // mode (octal, 8 bytes at 100): "0000644\0"
        giant[100..108].copy_from_slice(b"0000644\0");
        // uid/gid zero-filled octal
        giant[108..116].copy_from_slice(b"0000000\0");
        giant[116..124].copy_from_slice(b"0000000\0");
        // size field at 124, 12 bytes: a HUGE octal value (close to the field max).
        giant[124..135].copy_from_slice(b"77777777777"); // 11 sevens octal ≈ 8 GiB
        giant[135] = 0;
        // mtime at 136
        giant[136..147].copy_from_slice(b"00000000000");
        giant[147] = 0;
        // typeflag at 156 = '0' (regular)
        giant[156] = b'0';
        // Fix the checksum so the header parses (spaces in cksum field first).
        for b in giant.iter_mut().skip(148).take(8) {
            *b = b' ';
        }
        let sum: u32 = giant.iter().map(|&x| x as u32).sum();
        let cksum = format!("{:06o}\0 ", sum);
        giant[148..156].copy_from_slice(cksum.as_bytes());
        // Decode must reject on the size cap, NOT allocate ~8 GiB.
        let (_e2, err2) = decode_tar(&giant);
        assert!(
            err2.is_some(),
            "a huge declared size must produce a clean Tier-1 error, never OOM"
        );
        assert!(
            err2.as_ref().unwrap().contains("cap"),
            "the giant-size error must be the cap rejection, got: {:?}",
            err2
        );

        // Non-tar garbage.
        let garbage = vec![0xAB_u8; 4096];
        let (_e3, err3) = decode_tar(&garbage);
        // tar treats all-zero / non-tar as either empty or an error; either way no
        // panic. (All-0xAB is not a valid header → error.)
        assert!(err3.is_some() || _e3.is_empty());

        // Empty input.
        let (_e4, err4) = decode_tar(&[]);
        assert!(err4.is_some() || _e4.is_empty());
    }

    /// The MAX_ENTRY_BYTES cap is the alloc bound: anything over it is rejected
    /// BEFORE buffering.
    #[test]
    fn entry_size_cap_is_sane() {
        // The cap is non-zero and well under the generic 4 GiB alloc bound, so a
        // hostile declared size is rejected long before it could OOM.
        let cap = MAX_ENTRY_BYTES;
        assert!(cap > 0);
        assert!(cap < (MAX_ALLOC_COUNT as u64));
    }
}
