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
use std::io::{Read, Write};
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

/// The in-memory zip builder behind a [`NativeKind::ArchiveWriter`] handle. Mirrors
/// [`TarBuild`] — pure Rust over the vendored `zip` crate, no `Value` types. A
/// per-writer default compression method (`store=true` → no compression, else
/// deflate); each `add` may override it per entry.
pub struct ZipBuild {
    writer: zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    store: bool,
    finished: bool,
}

impl ZipBuild {
    pub(crate) fn new(store: bool) -> Self {
        ZipBuild {
            writer: zip::ZipWriter::new(std::io::Cursor::new(Vec::new())),
            store,
            finished: false,
        }
    }

    /// Append one entry. `dir=true` writes a directory entry (`add_directory`,
    /// empty data); otherwise a regular file with `data`. `store` (per-entry, falls
    /// back to the writer default) selects no-compression vs deflate. A failure is a
    /// `String` the caller surfaces however it likes.
    pub(crate) fn add(
        &mut self,
        name: &str,
        data: &[u8],
        mode: u32,
        store: bool,
        dir: bool,
    ) -> Result<(), String> {
        use zip::write::SimpleFileOptions;
        let method = if store {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(mode);
        if dir {
            self.writer
                .add_directory(name, opts)
                .map_err(|e| format!("archive add failed: {}", e))
        } else {
            self.writer
                .start_file(name, opts)
                .map_err(|e| format!("archive add failed: {}", e))?;
            self.writer
                .write_all(data)
                .map_err(|e| format!("archive add failed: {}", e))
        }
    }

    pub(crate) fn default_store(&self) -> bool {
        self.store
    }

    /// Finalize: flush the central directory. Consumes the builder.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>, String> {
        self.finished = true;
        let cursor = self
            .writer
            .finish()
            .map_err(|e| format!("archive finish failed: {}", e))?;
        Ok(cursor.into_inner())
    }
}

/// The `ResourceState` payload (interp.rs `ResourceState::ArchiveWriter`). An enum
/// so B2's zip writer joins without a new `ResourceState` variant.
pub enum ArchiveWriterState {
    Tar(TarBuild),
    // `ZipBuild` (a `zip::ZipWriter` over a cursor) is much larger than `TarBuild`,
    // so box it to keep the enum compact (clippy `large_enum_variant`).
    Zip(Box<ZipBuild>),
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

/// One decoded zip entry, before it becomes a `Value`. Carries `compressed_size`
/// (the on-disk compressed length, which `tar` has no equivalent of).
struct ZipEntry {
    name: String,
    size: u64,
    compressed_size: u64,
    mode: u32,
    is_dir: bool,
    data: Vec<u8>,
}

/// Decode every entry of a zip archive into a bounded in-memory list. Mirrors
/// [`decode_tar`]'s hostile-safe contract: a per-entry declared-size pre-check
/// against [`MAX_ENTRY_BYTES`] (a hostile `uncompressed_size` → immediate Tier-1,
/// no allocation) and incremental capped reads (a zip bomb whose deflate stream
/// lies stops at the cap, never OOM). A corrupt central directory / entry surfaces
/// as the trailing `Err`.
fn decode_zip(bytes: &[u8]) -> (Vec<ZipEntry>, Option<String>) {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(cursor) {
        Ok(a) => a,
        Err(e) => return (Vec::new(), Some(format!("archive read failed: {}", e))),
    };
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut file = match archive.by_index(i) {
            Ok(f) => f,
            Err(e) => return (out, Some(format!("archive entry header failed: {}", e))),
        };
        // Declared uncompressed size is the FIRST hostile gate — reject a giant
        // size BEFORE touching any buffer.
        let declared = file.size();
        if declared > MAX_ENTRY_BYTES {
            return (
                out,
                Some(format!(
                    "archive entry size {} exceeds {} byte cap",
                    declared, MAX_ENTRY_BYTES
                )),
            );
        }
        let name = file.name().to_string();
        let compressed_size = file.compressed_size();
        let is_dir = file.is_dir();
        let mode = file.unix_mode().unwrap_or(if is_dir { 0o755 } else { 0o644 });
        // Read incrementally, capped — a deflate stream that lies stops at the cap.
        let mut data = Vec::new();
        if let Err(e) = read_to_end_bounded(&mut file, &mut data, MAX_ENTRY_BYTES) {
            return (out, Some(format!("archive entry data failed: {}", e)));
        }
        let size = data.len() as u64;
        out.push(ZipEntry {
            name,
            size,
            compressed_size,
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

fn zip_entry_to_value(e: ZipEntry) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("name".to_string(), Value::str(e.name));
    m.insert("size".to_string(), Value::int(e.size as i64));
    m.insert(
        "compressedSize".to_string(),
        Value::int(e.compressed_size as i64),
    );
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
    let mut v = vec![
        ("tarWriter", bi("archive.tarWriter")),
        ("tarEntries", bi("archive.tarEntries")),
        ("tarAppend", bi("archive.tarAppend")),
        ("zipWriter", bi("archive.zipWriter")),
        ("zipEntries", bi("archive.zipEntries")),
    ];
    // The disk-touching helpers need `sys` (fs access) in addition to `archive`
    // (`compress`). When `sys` is off they are simply absent from the module
    // surface — the cap gate (`archive.*Extract*`/`*FromDir` → Fs) never fires
    // because the binding does not exist.
    #[cfg(feature = "sys")]
    {
        v.push(("tarExtractTo", bi("archive.tarExtractTo")));
        v.push(("zipExtractTo", bi("archive.zipExtractTo")));
        v.push(("tarCreateFromDir", bi("archive.tarCreateFromDir")));
    }
    v
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
        "zipWriter" => {
            let opts = arg(args, 0);
            let store = opt_bool(&opts, "store");
            let state = ResourceState::ArchiveWriter(Box::new(ArchiveWriterState::Zip(
                Box::new(ZipBuild::new(store)),
            )));
            Ok(interp.register_resource(NativeKind::ArchiveWriter, indexmap::IndexMap::new(), state))
        }
        "zipEntries" => {
            let raw = source_bytes(&arg(args, 0), span, "archive.zipEntries")?;
            Ok(make_zip_entries_generator(raw))
        }
        #[cfg(feature = "sys")]
        "tarExtractTo" => disk::tar_extract_to(args, span),
        #[cfg(feature = "sys")]
        "zipExtractTo" => disk::zip_extract_to(args, span),
        #[cfg(feature = "sys")]
        "tarCreateFromDir" => disk::tar_create_from_dir(args, span),
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

/// Build the lazy `zipEntries` generator (the zip analogue of
/// [`make_entries_generator`]). Decodes up-front into a bounded list, then yields
/// one `{name, size, compressedSize, mode, isDir, data}` per `next()`; a decode
/// error surfaces as a trailing Tier-1 pair.
fn make_zip_entries_generator(raw: Vec<u8>) -> Value {
    let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
        Box::pin(async move {
            let (entries, decode_err) = decode_zip(&raw);
            for e in entries {
                let g = current_generator().expect("inside a generator");
                g.yield_(zip_entry_to_value(e)).await;
            }
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
            let res = match state.as_mut() {
                ArchiveWriterState::Tar(build) => build.add(&name, &data, mode, mtime, dir),
                ArchiveWriterState::Zip(build) => {
                    // The per-entry `store` overrides the writer default; absent →
                    // the writer's default. `mode` defaulting differs for zip dirs.
                    let store = match opts.kind() {
                        ValueKind::Object(o) if o.get("store").is_some() => opt_bool(&opts, "store"),
                        _ => build.default_store(),
                    };
                    build.add(&name, &data, mode, store, dir)
                }
            };
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
            let res = match *state {
                ArchiveWriterState::Tar(build) => build.finish(),
                ArchiveWriterState::Zip(build) => build.finish(),
            };
            match res {
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

// ── zip-slip defense (spec §6.5 — the whole point of B2) ──────────────────────
//
// The single source of truth for "is this entry name safe to write under `dest`?".
// It is a PURELY LEXICAL component-wise normalization — it NEVER touches the
// filesystem (no `fs::canonicalize` on a not-yet-existing target → no TOCTOU). The
// disk extractors layer one more belt-and-suspenders check on top (a
// `write_path.starts_with(dest_canonical)` after the lexical join), and skip
// symlinks entirely by default so a malicious link can never be followed.

/// Why an entry name was rejected. The message names the offending entry so the
/// Tier-1 result is actionable (spec §6.5).
fn classify_entry_name(name: &str) -> Result<std::path::PathBuf, String> {
    // (1) NUL byte anywhere → reject (a NUL truncates a C path; never write it).
    if name.as_bytes().contains(&0) {
        return Err(format!("archive entry '{}' contains a NUL byte", name));
    }
    // (2) Absolute paths (POSIX `/`-leading, Windows drive `C:`, UNC `\\server`).
    let nb = name.as_bytes();
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(format!("archive entry '{}' is an absolute path", name));
    }
    // Windows drive letter `X:` or `X:\`.
    if nb.len() >= 2 && nb[1] == b':' && nb[0].is_ascii_alphabetic() {
        return Err(format!("archive entry '{}' is an absolute (drive) path", name));
    }
    // (3) Component-wise lexical normalization over BOTH separators. Track depth so
    //     a `..` that would escape the root (depth would go negative) is rejected
    //     WITHOUT consulting the filesystem.
    let mut parts: Vec<&str> = Vec::new();
    for raw in name.split(['/', '\\']) {
        match raw {
            // Empty component (leading/trailing/`a//b`) and `.` are no-ops.
            "" | "." => continue,
            ".." => {
                // Pop one real component; if there is none, this `..` escapes root.
                if parts.pop().is_none() {
                    return Err(format!(
                        "archive entry '{}' escapes the destination directory",
                        name
                    ));
                }
            }
            comp => parts.push(comp),
        }
    }
    // (4) A name that normalizes to nothing (`.`, `..`-balanced, all-empty) has no
    //     write target — reject so we never write the dest dir itself.
    if parts.is_empty() {
        return Err(format!(
            "archive entry '{}' has no valid path components",
            name
        ));
    }
    let mut rel = std::path::PathBuf::new();
    for p in parts {
        rel.push(p);
    }
    Ok(rel)
}

#[cfg(all(test, feature = "archive"))]
mod slip_tests {
    use super::classify_entry_name;

    /// Every hostile name class is rejected by the LEXICAL normalizer (no fs).
    #[test]
    fn classify_rejects_every_hostile_name() {
        let hostile = [
            "../evil",
            "../../evil",
            "/abs/evil",
            "..\\win",
            "C:\\x",
            "a/../../evil",
            "a/b/../../../evil",
            "evil\0.txt",
            ".",
            "..",
            "",
            "\\\\server\\share\\x", // UNC
        ];
        for h in hostile {
            assert!(
                classify_entry_name(h).is_err(),
                "hostile name {:?} must be rejected",
                h
            );
        }
    }

    /// A `..` that stays WITHIN the tree (e.g. `a/b/../c` → `a/c`) is allowed and
    /// normalizes correctly; a benign nested name passes through.
    #[test]
    fn classify_allows_safe_names() {
        assert_eq!(
            classify_entry_name("a/b/../c").unwrap(),
            std::path::Path::new("a/c")
        );
        assert_eq!(
            classify_entry_name("dir/sub/file.txt").unwrap(),
            std::path::Path::new("dir/sub/file.txt")
        );
        assert_eq!(
            classify_entry_name("./clean.txt").unwrap(),
            std::path::Path::new("clean.txt")
        );
    }
}

#[cfg(all(feature = "archive", feature = "sys"))]
mod disk {
    //! Fs-gated disk extraction + create-from-dir (spec §6.5). Gated on BOTH
    //! `archive` (=`compress`, the codecs) AND `sys` (filesystem access). The cap
    //! gate (`archive.{tarExtractTo,zipExtractTo,tarCreateFromDir}` → `Cap::Fs`,
    //! wired in B1) fires BEFORE we reach here, so under `--sandbox`/`--deny fs`
    //! these are denied without ever touching the disk.

    use super::*;
    use std::path::{Path, PathBuf};

    /// What to do with a symlink/hardlink entry: skip it (the safe default — never
    /// materialize a link that could be followed later, §6.5) or fail the whole
    /// extract with a Tier-1 error.
    pub(super) enum LinkPolicy {
        Skip,
        Error,
    }

    fn link_policy(opts: &Value) -> LinkPolicy {
        if let ValueKind::Object(o) = opts.kind() {
            if let Some(v) = o.get("links") {
                if let ValueKind::Str(s) = v.kind() {
                    if s.as_ref() == "error" {
                        return LinkPolicy::Error;
                    }
                }
            }
        }
        LinkPolicy::Skip
    }

    /// Canonicalize `dest` (it MUST exist — we create it if missing first) into the
    /// jail root. Returns the canonical dest or a Tier-1 message.
    fn jail_root(dest: &str) -> Result<PathBuf, String> {
        let p = Path::new(dest);
        if !p.exists() {
            std::fs::create_dir_all(p)
                .map_err(|e| format!("archive extract: cannot create dest '{}': {}", dest, e))?;
        }
        std::fs::canonicalize(p)
            .map_err(|e| format!("archive extract: cannot resolve dest '{}': {}", dest, e))
    }

    /// Compute + VALIDATE the write path for one entry name under the canonical
    /// jail root. The lexical normalization rejects `..`/absolute/drive/UNC/NUL;
    /// the `starts_with` after the join is the belt-and-suspenders re-check that a
    /// pre-existing symlinked subdir in the path cannot let the write escape (we
    /// join onto the CANONICAL root, never resolving through a symlink).
    fn safe_write_path(root: &Path, name: &str) -> Result<(PathBuf, PathBuf), String> {
        let rel = classify_entry_name(name)?;
        let joined = root.join(&rel);
        // Belt-and-suspenders: the lexically-joined path MUST be under the root.
        // (Lexical, since neither `joined` nor its ancestors may exist yet — we do
        // NOT canonicalize the target.)
        if !joined.starts_with(root) {
            return Err(format!(
                "archive entry '{}' escapes the destination directory",
                name
            ));
        }
        Ok((joined, rel))
    }

    /// Create every intermediate directory from `root` down to `path` (exclusive of
    /// `path` itself) **without ever traversing a symlink** (spec §6.5 second-order
    /// defense). Each component under the jail root: if it EXISTS and is a symlink →
    /// reject (a pre-existing symlinked subdir is the write-through vector); if it
    /// does not exist → `create_dir` it; if it's a real dir → descend. Because we
    /// start from the canonical root and check each link, the final write cannot
    /// escape through a planted or pre-existing symlink. `rel` is the validated
    /// relative path (`..`/absolute already rejected).
    fn make_dirs_nofollow(root: &Path, rel: &Path, leaf_is_dir: bool) -> Result<(), String> {
        let comps: Vec<_> = rel.components().collect();
        let last = comps.len();
        let mut cur = root.to_path_buf();
        for (i, comp) in comps.iter().enumerate() {
            // For a file entry, the final component is the file itself — stop before
            // it (the write creates it). For a dir entry, create the final too.
            if i + 1 == last && !leaf_is_dir {
                break;
            }
            cur.push(comp);
            match std::fs::symlink_metadata(&cur) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(format!(
                            "archive extract: path component '{}' is a symlink (refused)",
                            cur.display()
                        ));
                    }
                    if !meta.is_dir() {
                        return Err(format!(
                            "archive extract: path component '{}' is not a directory",
                            cur.display()
                        ));
                    }
                }
                Err(_) => {
                    std::fs::create_dir(&cur)
                        .map_err(|e| format!("archive extract: mkdir failed: {}", e))?;
                }
            }
        }
        Ok(())
    }

    /// Tracks files/dirs we created so a mid-extract rejection can best-effort clean
    /// up the partial output (spec §6.5 — fail on the FIRST hostile entry, never
    /// leave a partial-then-error).
    struct Written {
        files: Vec<PathBuf>,
    }

    impl Written {
        fn new() -> Self {
            Written { files: Vec::new() }
        }
        fn rollback(&self) {
            for f in self.files.iter().rev() {
                let _ = std::fs::remove_file(f);
            }
        }
    }

    /// Write one regular-file entry's data to `path`, creating parent dirs through
    /// [`make_dirs_nofollow`] (which refuses to traverse a symlink). The leaf is
    /// also checked: if it already exists AS a symlink, we refuse (never write
    /// THROUGH it). `rel` is the validated relative path (used for the no-follow
    /// parent walk from `root`).
    fn write_entry(
        root: &Path,
        rel: &Path,
        path: &Path,
        data: &[u8],
        mode: u32,
        written: &mut Written,
    ) -> Result<(), String> {
        make_dirs_nofollow(root, rel, false)?;
        // Refuse to write through a pre-existing symlink at the leaf.
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "archive extract: target '{}' is a symlink (refused)",
                    path.display()
                ));
            }
        }
        std::fs::write(path, data)
            .map_err(|e| format!("archive extract: write failed: {}", e))?;
        written.files.push(path.to_path_buf());
        // Best-effort mode (unix only; ignored elsewhere).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
        #[cfg(not(unix))]
        let _ = mode;
        Ok(())
    }

    fn dest_arg(args: &[Value], span: Span, ctx: &str) -> Result<String, Control> {
        match arg(args, 1).kind() {
            ValueKind::Str(s) => Ok(s.to_string()),
            other => Err(AsError::at(
                format!("{} dest must be a string, got {}", ctx, type_name_of(other)),
                span,
            )
            .into()),
        }
    }

    fn type_name_of(_k: ValueKind) -> &'static str {
        "value"
    }

    /// `tarExtractTo(bytes, dest, opts?)` — extract a (possibly gzipped) tar to
    /// `dest`, with the full zip-slip defense + symlink/hardlink handling.
    pub(super) fn tar_extract_to(args: &[Value], span: Span) -> Result<Value, Control> {
        let raw = source_bytes(&arg(args, 0), span, "archive.tarExtractTo")?;
        let dest = dest_arg(args, span, "archive.tarExtractTo")?;
        let opts = arg(args, 2);
        let policy = link_policy(&opts);
        Ok(run_tar_extract(&raw, &dest, policy))
    }

    fn run_tar_extract(raw: &[u8], dest: &str, policy: LinkPolicy) -> Value {
        let root = match jail_root(dest) {
            Ok(r) => r,
            Err(e) => return make_pair(Value::nil(), make_error(Value::str(e))),
        };
        // Decompress (bounded) then iterate entries, validating EACH before write.
        let decompressed = match maybe_gunzip(raw) {
            Ok(d) => d,
            Err(e) => return make_pair(Value::nil(), make_error(Value::str(e))),
        };
        let mut archive = tar::Archive::new(std::io::Cursor::new(decompressed));
        let entries = match archive.entries() {
            Ok(it) => it,
            Err(e) => {
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("archive read failed: {}", e))),
                )
            }
        };
        let mut written = Written::new();
        for entry in entries {
            let mut entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    written.rollback();
                    return make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("archive entry header failed: {}", e))),
                    );
                }
            };
            let name = match entry.path() {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(e) => {
                    written.rollback();
                    return make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("archive entry path failed: {}", e))),
                    );
                }
            };
            let etype = entry.header().entry_type();
            // Symlinks AND hardlinks are the second-order zip-slip vector — by
            // default skip them (never materialize), and ALSO validate that their
            // link target does not escape so `{links:"error"}` names the offender.
            if etype.is_symlink() || etype.is_hard_link() {
                match policy {
                    LinkPolicy::Skip => continue,
                    LinkPolicy::Error => {
                        written.rollback();
                        return make_pair(
                            Value::nil(),
                            make_error(Value::str(format!(
                                "archive entry '{}' is a link (refused: links=\"error\")",
                                name
                            ))),
                        );
                    }
                }
            }
            // The size pre-check (hostile declared size → no alloc).
            let declared = entry.header().size().unwrap_or(0);
            if declared > MAX_ENTRY_BYTES {
                written.rollback();
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!(
                        "archive entry size {} exceeds {} byte cap",
                        declared, MAX_ENTRY_BYTES
                    ))),
                );
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            if etype.is_dir() {
                // A directory entry: validate the name, then mkdir it (no-follow).
                let (_path, rel) = match safe_write_path(&root, &name) {
                    Ok(p) => p,
                    Err(e) => {
                        written.rollback();
                        return make_pair(Value::nil(), make_error(Value::str(e)));
                    }
                };
                if let Err(e) = make_dirs_nofollow(&root, &rel, true) {
                    written.rollback();
                    return make_pair(Value::nil(), make_error(Value::str(e)));
                }
                continue;
            }
            // Regular file: validate name FIRST (the hostile gate), then read data.
            let (path, rel) = match safe_write_path(&root, &name) {
                Ok(p) => p,
                Err(e) => {
                    written.rollback();
                    return make_pair(Value::nil(), make_error(Value::str(e)));
                }
            };
            let mut data = Vec::new();
            if let Err(e) = read_to_end_bounded(&mut entry, &mut data, MAX_ENTRY_BYTES) {
                written.rollback();
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("archive entry data failed: {}", e))),
                );
            }
            if let Err(e) = write_entry(&root, &rel, &path, &data, mode, &mut written) {
                written.rollback();
                return make_pair(Value::nil(), make_error(Value::str(e)));
            }
        }
        make_pair(Value::str(dest.to_string()), Value::nil())
    }

    /// `zipExtractTo(bytes, dest, opts?)` — the zip analogue of `tarExtractTo`.
    pub(super) fn zip_extract_to(args: &[Value], span: Span) -> Result<Value, Control> {
        let raw = source_bytes(&arg(args, 0), span, "archive.zipExtractTo")?;
        let dest = dest_arg(args, span, "archive.zipExtractTo")?;
        let opts = arg(args, 2);
        let policy = link_policy(&opts);
        Ok(run_zip_extract(&raw, &dest, policy))
    }

    fn run_zip_extract(raw: &[u8], dest: &str, policy: LinkPolicy) -> Value {
        let root = match jail_root(dest) {
            Ok(r) => r,
            Err(e) => return make_pair(Value::nil(), make_error(Value::str(e))),
        };
        let cursor = std::io::Cursor::new(raw);
        let mut zarc = match zip::ZipArchive::new(cursor) {
            Ok(a) => a,
            Err(e) => {
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("archive read failed: {}", e))),
                )
            }
        };
        let mut written = Written::new();
        for i in 0..zarc.len() {
            let mut file = match zarc.by_index(i) {
                Ok(f) => f,
                Err(e) => {
                    written.rollback();
                    return make_pair(
                        Value::nil(),
                        make_error(Value::str(format!("archive entry header failed: {}", e))),
                    );
                }
            };
            let name = file.name().to_string();
            // A zip symlink is a regular entry with the S_IFLNK mode bit; treat any
            // such entry as a link under the same policy (second-order defense).
            let is_symlink = file
                .unix_mode()
                .map(|m| (m & 0o170000) == 0o120000)
                .unwrap_or(false);
            if is_symlink {
                match policy {
                    LinkPolicy::Skip => continue,
                    LinkPolicy::Error => {
                        written.rollback();
                        return make_pair(
                            Value::nil(),
                            make_error(Value::str(format!(
                                "archive entry '{}' is a link (refused: links=\"error\")",
                                name
                            ))),
                        );
                    }
                }
            }
            let declared = file.size();
            if declared > MAX_ENTRY_BYTES {
                written.rollback();
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!(
                        "archive entry size {} exceeds {} byte cap",
                        declared, MAX_ENTRY_BYTES
                    ))),
                );
            }
            let mode = file.unix_mode().unwrap_or(0o644);
            if file.is_dir() {
                let (_path, rel) = match safe_write_path(&root, &name) {
                    Ok(p) => p,
                    Err(e) => {
                        written.rollback();
                        return make_pair(Value::nil(), make_error(Value::str(e)));
                    }
                };
                if let Err(e) = make_dirs_nofollow(&root, &rel, true) {
                    written.rollback();
                    return make_pair(Value::nil(), make_error(Value::str(e)));
                }
                continue;
            }
            let (path, rel) = match safe_write_path(&root, &name) {
                Ok(p) => p,
                Err(e) => {
                    written.rollback();
                    return make_pair(Value::nil(), make_error(Value::str(e)));
                }
            };
            let mut data = Vec::new();
            if let Err(e) = read_to_end_bounded(&mut file, &mut data, MAX_ENTRY_BYTES) {
                written.rollback();
                return make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("archive entry data failed: {}", e))),
                );
            }
            if let Err(e) = write_entry(&root, &rel, &path, &data, mode, &mut written) {
                written.rollback();
                return make_pair(Value::nil(), make_error(Value::str(e)));
            }
        }
        make_pair(Value::str(dest.to_string()), Value::nil())
    }

    /// `tarCreateFromDir(dir, opts?)` — walk `dir` and build a tar of its contents.
    /// `{deterministic:true}` zeroes per-entry mtime/uid/gid AND sorts entries by
    /// path, so two runs over the same tree produce byte-identical bytes. Symlinks
    /// inside the tree are SKIPPED (never embedded — mirrors the extraction defense).
    pub(super) fn tar_create_from_dir(args: &[Value], span: Span) -> Result<Value, Control> {
        let dir = match arg(args, 0).kind() {
            ValueKind::Str(s) => s.to_string(),
            other => {
                return Err(AsError::at(
                    format!(
                        "archive.tarCreateFromDir dir must be a string, got {}",
                        type_name_of(other)
                    ),
                    span,
                )
                .into())
            }
        };
        let opts = arg(args, 1);
        let gzip = opt_bool(&opts, "gzip");
        let deterministic = opt_bool(&opts, "deterministic");
        Ok(run_tar_create_from_dir(&dir, gzip, deterministic))
    }

    fn run_tar_create_from_dir(dir: &str, gzip: bool, deterministic: bool) -> Value {
        let root = Path::new(dir);
        let mut collected: Vec<(String, bool, Vec<u8>, u32, u64)> = Vec::new();
        if let Err(e) = collect_dir(root, root, deterministic, &mut collected) {
            return make_pair(Value::nil(), make_error(Value::str(e)));
        }
        if deterministic {
            // Sort by the archive name for byte-stable ordering across runs.
            collected.sort_by(|a, b| a.0.cmp(&b.0));
        }
        let mut build = TarBuild::new(gzip, deterministic);
        for (name, is_dir, data, mode, mtime) in collected {
            if let Err(e) = build.add(&name, &data, mode, mtime, is_dir) {
                return make_pair(Value::nil(), make_error(Value::str(e)));
            }
        }
        match build.finish() {
            Ok(b) => make_pair(bytes_val(b), Value::nil()),
            Err(e) => make_pair(Value::nil(), make_error(Value::str(e))),
        }
    }

    /// Recursively collect regular files + dirs under `root`, recording the relative
    /// archive name. Symlinks are skipped (`symlink_metadata` so we never follow).
    #[allow(clippy::type_complexity)]
    fn collect_dir(
        root: &Path,
        cur: &Path,
        deterministic: bool,
        out: &mut Vec<(String, bool, Vec<u8>, u32, u64)>,
    ) -> Result<(), String> {
        let rd = std::fs::read_dir(cur)
            .map_err(|e| format!("archive.tarCreateFromDir read failed: {}", e))?;
        for ent in rd {
            let ent = ent.map_err(|e| format!("archive.tarCreateFromDir read failed: {}", e))?;
            let path = ent.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("archive.tarCreateFromDir stat failed: {}", e))?;
            // Never follow / embed a symlink (mirrors the extraction defense).
            if meta.file_type().is_symlink() {
                continue;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let mode = file_mode(&meta);
            let mtime = if deterministic { 0 } else { file_mtime(&meta) };
            if meta.is_dir() {
                out.push((format!("{}/", rel), true, Vec::new(), mode, mtime));
                collect_dir(root, &path, deterministic, out)?;
            } else if meta.is_file() {
                let data = std::fs::read(&path)
                    .map_err(|e| format!("archive.tarCreateFromDir read failed: {}", e))?;
                if data.len() as u64 > MAX_ENTRY_BYTES {
                    return Err(format!(
                        "archive.tarCreateFromDir file '{}' exceeds {} byte cap",
                        rel, MAX_ENTRY_BYTES
                    ));
                }
                out.push((rel, false, data, mode, mtime));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn file_mode(meta: &std::fs::Metadata) -> u32 {
        use std::os::unix::fs::MetadataExt;
        meta.mode() & 0o7777
    }
    #[cfg(not(unix))]
    fn file_mode(meta: &std::fs::Metadata) -> u32 {
        if meta.is_dir() {
            0o755
        } else {
            0o644
        }
    }

    fn file_mtime(meta: &std::fs::Metadata) -> u64 {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    // ── test-only accessors (the zip_slip_battery drives these directly) ────────
    #[cfg(test)]
    pub(super) fn test_policy_skip() -> LinkPolicy {
        LinkPolicy::Skip
    }
    #[cfg(test)]
    pub(super) fn test_policy_error() -> LinkPolicy {
        LinkPolicy::Error
    }
    #[cfg(test)]
    pub(super) fn run_tar_extract_for_test(raw: &[u8], dest: &str, policy: LinkPolicy) -> Value {
        run_tar_extract(raw, dest, policy)
    }
    #[cfg(test)]
    pub(super) fn run_zip_extract_for_test(raw: &[u8], dest: &str, policy: LinkPolicy) -> Value {
        run_zip_extract(raw, dest, policy)
    }
    #[cfg(test)]
    pub(super) fn run_tar_create_from_dir_for_test(
        dir: &str,
        gzip: bool,
        deterministic: bool,
    ) -> Value {
        run_tar_create_from_dir(dir, gzip, deterministic)
    }
}

/// Test-only thin wrappers over the `disk` extractors, calling them with forged
/// bytes + a dest path without the `Value` arg-marshalling layer. Returns the same
/// Tier-1 `Value` pair the public fns return so the battery can assert on it.
#[cfg(all(test, feature = "archive", feature = "sys"))]
mod disk_test_api {
    use super::*;

    pub(super) fn tar_extract(bytes: &[u8], dest: &str, links_error: bool) -> Value {
        let policy = if links_error {
            super::disk::test_policy_error()
        } else {
            super::disk::test_policy_skip()
        };
        super::disk::run_tar_extract_for_test(bytes, dest, policy)
    }

    pub(super) fn zip_extract(bytes: &[u8], dest: &str, links_error: bool) -> Value {
        let policy = if links_error {
            super::disk::test_policy_error()
        } else {
            super::disk::test_policy_skip()
        };
        super::disk::run_zip_extract_for_test(bytes, dest, policy)
    }

    pub(super) fn tar_create_from_dir_bytes(dir: &str, deterministic: bool) -> Option<Vec<u8>> {
        let v = super::disk::run_tar_create_from_dir_for_test(dir, false, deterministic);
        if let ValueKind::Array(a) = v.kind() {
            let a = a.borrow();
            if let ValueKind::Bytes(b) = a[0].kind() {
                return Some(b.borrow().clone());
            }
        }
        None
    }

    pub(super) fn forge_zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut b = ZipBuild::new(false);
        for (name, data, mode) in entries {
            b.add(name, data, *mode, false, false).unwrap();
        }
        b.finish().unwrap()
    }

    /// `[v, nil]` → success.
    pub(super) fn is_ok_tier1(v: &Value) -> bool {
        if let ValueKind::Array(a) = v.kind() {
            let a = a.borrow();
            return a.len() == 2 && matches!(a[1].kind(), ValueKind::Nil);
        }
        false
    }

    /// `[nil, err]` → error.
    pub(super) fn is_tier1_err(v: &Value) -> bool {
        if let ValueKind::Array(a) = v.kind() {
            let a = a.borrow();
            return a.len() == 2
                && matches!(a[0].kind(), ValueKind::Nil)
                && matches!(a[1].kind(), ValueKind::Object(_));
        }
        false
    }

    /// Assert a Tier-1 error whose message names the offending entry.
    pub(super) fn assert_tier1_naming(v: &Value, name: &str) {
        assert!(is_tier1_err(v), "expected a [nil, err] pair, got {:?}", v);
        if let ValueKind::Array(a) = v.kind() {
            let a = a.borrow();
            if let ValueKind::Object(o) = a[1].kind() {
                if let Some(msg) = o.get("message") {
                    if let ValueKind::Str(s) = msg.kind() {
                        assert!(
                            s.contains(name),
                            "error message {:?} must name the offending entry {:?}",
                            s.as_ref(),
                            name
                        );
                        return;
                    }
                }
            }
        }
        panic!("error pair had no string message naming the entry");
    }
}

#[cfg(all(test, feature = "archive", feature = "sys"))]
mod zip_slip_battery {
    //! THE load-bearing security test (spec §6.5). Constructs hostile archives
    //! IN the test and proves every vector is rejected AND nothing is written
    //! outside `dest`. Calls the disk extractors directly (the cap gate is proven
    //! separately in `tests/cap_audit.rs`).

    use super::disk_test_api::*;
    use std::path::{Path, PathBuf};

    /// A throwaway tempdir with a sibling "outside" sentinel. We assert that after
    /// each hostile extract, NO file landed outside `dest` by walking the PARENT.
    struct Sandbox {
        base: PathBuf, // the parent we walk to detect escapes
        dest: PathBuf, // the extraction destination
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "ascript_slip_{}_{}",
                tag,
                std::process::id() as u64 * 1000 + rand_suffix()
            ));
            let dest = base.join("dest");
            std::fs::create_dir_all(&dest).unwrap();
            Sandbox { base, dest }
        }
        fn dest_str(&self) -> String {
            self.dest.to_string_lossy().into_owned()
        }
        /// Every file under `base` MUST be under `dest` (nothing escaped).
        fn assert_no_escape(&self) {
            let canon_dest = std::fs::canonicalize(&self.dest).unwrap();
            walk_assert(&self.base, &canon_dest);
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    fn walk_assert(dir: &Path, canon_dest: &Path) {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for ent in rd.flatten() {
            let path = ent.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                // A symlink should NEVER be created by extraction (skip default).
                panic!("a symlink was materialized at {:?} — link defense breached", path);
            }
            if meta.is_dir() {
                walk_assert(&path, canon_dest);
            } else {
                let canon = std::fs::canonicalize(&path).unwrap();
                assert!(
                    canon.starts_with(canon_dest),
                    "file {:?} escaped dest {:?}",
                    canon,
                    canon_dest
                );
            }
        }
    }

    /// Build a tar with arbitrary (name, type, link target) entries — used to forge
    /// hostile archives the high-level writer would refuse to create.
    fn forge_tar(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, etype, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*etype);
            h.set_mode(0o644);
            h.set_mtime(0);
            if let Some(target) = link {
                h.set_size(0);
                builder.append_link(&mut h, name, target).unwrap();
            } else {
                h.set_size(data.len() as u64);
                builder.append_data(&mut h, name, &data[..]).unwrap();
            }
        }
        builder.into_inner().unwrap()
    }

    /// Forge a tar with a RAW header so we can embed a name the `tar` crate's writer
    /// would reject (`..`, absolute, drive, NUL). One regular-file entry, USTAR.
    fn forge_tar_raw_name(name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut h = [0u8; 512];
        // name (offset 0, 100 bytes) — truncated to fit; embed raw bytes verbatim.
        let nlen = name.len().min(100);
        h[..nlen].copy_from_slice(&name[..nlen]);
        // mode/uid/gid (octal, NUL-terminated)
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        // size (12 bytes octal)
        let size_oct = format!("{:011o}\0", data.len());
        h[124..136].copy_from_slice(size_oct.as_bytes());
        // mtime
        h[136..148].copy_from_slice(b"00000000000\0");
        // typeflag '0' = regular
        h[156] = b'0';
        // ustar magic
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        // checksum: fill the field with spaces, sum all bytes, then write it.
        for b in h.iter_mut().skip(148).take(8) {
            *b = b' ';
        }
        let sum: u32 = h.iter().map(|&x| x as u32).sum();
        let cksum = format!("{:06o}\0 ", sum);
        h[148..156].copy_from_slice(cksum.as_bytes());
        out.extend_from_slice(&h);
        // data block(s), zero-padded to 512.
        out.extend_from_slice(data);
        let pad = (512 - (data.len() % 512)) % 512;
        out.resize(out.len() + pad, 0u8);
        // two zero blocks = end of archive.
        out.resize(out.len() + 1024, 0u8);
        out
    }

    // ── (1) name-based vectors ──────────────────────────────────────────────────

    #[test]
    fn tar_name_vectors_all_rejected_nothing_escapes() {
        let hostile_names: &[&str] = &[
            "../evil",
            "../../evil",
            "/abs/evil",
            "..\\win",
            "C:\\x",
            "a/../../evil",
            "a/b/../../../evil",
            ".",
            "..",
        ];
        for name in hostile_names {
            let sb = Sandbox::new("tarname");
            let bytes = forge_tar_raw_name(name.as_bytes(), b"pwn");
            let r = tar_extract(&bytes, &sb.dest_str(), false);
            assert_tier1_naming(&r, name);
            sb.assert_no_escape();
        }
    }

    #[test]
    fn nul_name_rejected_or_safely_truncated() {
        // The tar header name field is NUL-TERMINATED by the USTAR spec, so the tar
        // reader reads `evil\0.txt` as `evil` — a SAFE name written under dest (no
        // NUL ever reaches the fs). The defense that matters: nothing escapes.
        let sb = Sandbox::new("tarnul");
        let bytes = forge_tar_raw_name(b"evil\0.txt", b"pwn");
        let r = tar_extract(&bytes, &sb.dest_str(), false);
        // Either rejected, or written as the truncated-safe `evil` — never a NUL on
        // disk, never an escape.
        assert!(is_ok_tier1(&r) || is_tier1_err(&r));
        assert!(
            !sb.dest.join("evil\0.txt").exists(),
            "a NUL-bearing path must never be created"
        );
        sb.assert_no_escape();

        // The zip path, by contrast, carries the NUL into the name string — our
        // `classify_entry_name` NUL gate REJECTS it (Tier-1 naming the entry).
        let sb2 = Sandbox::new("zipnul");
        let zbytes = forge_zip(&[("evil\0.txt", b"pwn", 0o644)]);
        let r2 = zip_extract(&zbytes, &sb2.dest_str(), false);
        assert!(is_tier1_err(&r2), "a NUL zip name must be rejected");
        sb2.assert_no_escape();
    }

    #[test]
    fn zip_name_vectors_all_rejected_nothing_escapes() {
        let hostile_names: &[&str] = &[
            "../evil",
            "../../evil",
            "a/b/../../../evil",
            "..\\win",
            ".",
            "..",
        ];
        for name in hostile_names {
            let sb = Sandbox::new("zipname");
            let bytes = forge_zip(&[(name, b"pwn", 0o644)]);
            let r = zip_extract(&bytes, &sb.dest_str(), false);
            assert_tier1_naming(&r, name);
            sb.assert_no_escape();
        }
    }

    // ── (2) symlink / hardlink vectors ──────────────────────────────────────────

    #[test]
    fn tar_symlink_outside_skipped_by_default() {
        let sb = Sandbox::new("tarsymskip");
        let bytes = forge_tar(&[("link", tar::EntryType::Symlink, b"", Some("/tmp"))]);
        let r = tar_extract(&bytes, &sb.dest_str(), false);
        // Skip-by-default → success, but NO symlink materialized.
        assert!(is_ok_tier1(&r), "symlink-skip default must succeed");
        assert!(
            !sb.dest.join("link").exists(),
            "the symlink must NOT have been created"
        );
        sb.assert_no_escape();
    }

    #[test]
    fn tar_symlink_links_error_is_tier1() {
        let sb = Sandbox::new("tarsymerr");
        let bytes = forge_tar(&[("link", tar::EntryType::Symlink, b"", Some("../escape"))]);
        let r = tar_extract(&bytes, &sb.dest_str(), true); // links="error"
        assert!(is_tier1_err(&r), "links=error must reject a symlink entry");
        sb.assert_no_escape();
    }

    #[test]
    fn tar_hardlink_outside_skipped_by_default() {
        let sb = Sandbox::new("tarhard");
        let bytes = forge_tar(&[("hl", tar::EntryType::Link, b"", Some("/etc/passwd"))]);
        let r = tar_extract(&bytes, &sb.dest_str(), false);
        assert!(is_ok_tier1(&r));
        assert!(!sb.dest.join("hl").exists(), "hardlink must NOT be created");
        sb.assert_no_escape();
    }

    // ── (3) SECOND-ORDER zip-slip: link then write-through it ───────────────────

    #[test]
    fn tar_second_order_symlink_then_write_through() {
        let sb = Sandbox::new("tar2nd");
        // entry 1: a symlink `link` -> /tmp ; entry 2: `link/evil.txt`.
        // The naive extractor creates `link`, then `link/evil.txt` lands in /tmp.
        // Our defense: entry 1 is SKIPPED (no link created); entry 2 then lands at
        // dest/link/evil.txt (a real dir+file under dest — NOT an escape). Either
        // way nothing escapes and no symlink is materialized.
        let bytes = forge_tar(&[
            ("link", tar::EntryType::Symlink, b"", Some("/tmp")),
            ("link/evil.txt", tar::EntryType::Regular, b"pwn", None),
        ]);
        let r = tar_extract(&bytes, &sb.dest_str(), false);
        assert!(is_ok_tier1(&r) || is_tier1_err(&r));
        // The symlink must NOT exist; if link/evil.txt was written it's a REAL dir.
        let link_path = sb.dest.join("link");
        if link_path.exists() {
            let meta = std::fs::symlink_metadata(&link_path).unwrap();
            assert!(
                !meta.file_type().is_symlink(),
                "second-order: `link` must be a real dir, never a symlink"
            );
        }
        sb.assert_no_escape();
    }

    #[cfg(unix)]
    #[test]
    fn tar_write_through_preexisting_symlinked_dir() {
        // dest contains a PRE-EXISTING symlinked subdir `sub` -> an outside dir.
        // An entry `sub/evil.txt` must NOT be written through the symlink. Our
        // lexical-join-from-canonical-root + starts_with means we compute
        // dest/sub/evil.txt; create_dir_all(dest/sub) — but `sub` already exists as
        // a symlink, so create_dir_all is a no-op and write() WOULD follow it.
        // The defense: we extract to the CANONICAL dest and the write path is under
        // it lexically; the pre-existing symlink is the host's concern, but we must
        // never let the WRITE escape. We assert the outside dir stays empty.
        let sb = Sandbox::new("tarpre");
        let outside = sb.base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, sb.dest.join("sub")).unwrap();
        let bytes = forge_tar(&[("sub/evil.txt", tar::EntryType::Regular, b"pwn", None)]);
        let _r = tar_extract(&bytes, &sb.dest_str(), false);
        // The outside dir must remain empty — the write did not escape through the
        // pre-existing symlink.
        let escaped = std::fs::read_dir(&outside)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        assert!(!escaped, "write escaped through a pre-existing symlinked dir");
    }

    // ── happy paths ─────────────────────────────────────────────────────────────

    #[test]
    fn tar_happy_path_nested_dirs_and_modes() {
        let sb = Sandbox::new("tarhappy");
        let bytes = forge_tar(&[
            ("a/b/", tar::EntryType::Directory, b"", None),
            ("a/b/c.txt", tar::EntryType::Regular, b"hello", None),
            ("top.txt", tar::EntryType::Regular, b"top", None),
        ]);
        let r = tar_extract(&bytes, &sb.dest_str(), false);
        assert!(is_ok_tier1(&r), "happy path must succeed: {:?}", r);
        assert_eq!(std::fs::read(sb.dest.join("a/b/c.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(sb.dest.join("top.txt")).unwrap(), b"top");
        sb.assert_no_escape();
    }

    #[test]
    fn zip_happy_path() {
        let sb = Sandbox::new("ziphappy");
        let bytes = forge_zip(&[("dir/x.txt", b"zdata", 0o644)]);
        let r = zip_extract(&bytes, &sb.dest_str(), false);
        assert!(is_ok_tier1(&r), "zip happy path must succeed: {:?}", r);
        assert_eq!(std::fs::read(sb.dest.join("dir/x.txt")).unwrap(), b"zdata");
        sb.assert_no_escape();
    }

    #[test]
    fn tar_create_from_dir_is_deterministic() {
        // Build a small tree, archive it deterministically twice → identical bytes.
        let root = std::env::temp_dir().join(format!("ascript_tcfd_{}", rand_suffix()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"beta").unwrap();
        let p = root.to_string_lossy().into_owned();
        let a = tar_create_from_dir_bytes(&p, true);
        let b = tar_create_from_dir_bytes(&p, true);
        std::fs::remove_dir_all(&root).ok();
        assert!(a.is_some() && b.is_some(), "create must succeed");
        assert_eq!(a, b, "deterministic create must be byte-stable across runs");
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

    // ── ZIP plane round-trips (B2) ───────────────────────────────────────────

    /// zipWriter → add → finish → decode_zip round-trips names/sizes/data +
    /// reports a sane compressedSize.
    #[test]
    fn zip_writer_roundtrip_names_sizes_data() {
        let mut b = ZipBuild::new(false); // deflate default
        b.add("a.txt", b"hello world", 0o644, false, false).unwrap();
        b.add("dir/", &[], 0o755, false, true).unwrap();
        b.add("b.bin", &[1, 2, 3, 4, 5], 0o600, false, false).unwrap();
        let bytes = b.finish().unwrap();

        let (entries, err) = decode_zip(&bytes);
        assert!(err.is_none(), "clean zip must decode without error");
        // dir entry may or may not be enumerated as a separate index; assert the
        // two files are present with correct data.
        let a = entries.iter().find(|e| e.name == "a.txt").expect("a.txt");
        assert_eq!(a.size, 11);
        assert_eq!(a.data, b"hello world");
        assert!(!a.is_dir);
        let bb = entries.iter().find(|e| e.name == "b.bin").expect("b.bin");
        assert_eq!(bb.data, vec![1, 2, 3, 4, 5]);
        // compressedSize is populated (non-panicking; >0 for non-empty deflate).
        assert!(bb.compressed_size > 0);
    }

    /// `{store:true}` (no compression) vs deflate: a store entry's compressedSize
    /// equals its uncompressed size; deflate of compressible data is smaller.
    #[test]
    fn zip_store_vs_deflate() {
        // A highly-compressible payload.
        let payload = vec![b'A'; 4096];

        let mut stored = ZipBuild::new(true); // store default
        stored.add("s.txt", &payload, 0o644, true, false).unwrap();
        let sbytes = stored.finish().unwrap();
        let (sent, serr) = decode_zip(&sbytes);
        assert!(serr.is_none());
        let s = &sent[0];
        assert_eq!(s.size, 4096);
        // Stored: compressed == uncompressed.
        assert_eq!(s.compressed_size, 4096);

        let mut defl = ZipBuild::new(false);
        defl.add("d.txt", &payload, 0o644, false, false).unwrap();
        let dbytes = defl.finish().unwrap();
        let (dent, derr) = decode_zip(&dbytes);
        assert!(derr.is_none());
        let d = &dent[0];
        assert_eq!(d.size, 4096);
        assert!(
            d.compressed_size < 4096,
            "deflate of 4 KiB of 'A' must shrink, got {}",
            d.compressed_size
        );
    }

    /// Hostile zip input → clean Tier-1, never a panic/OOM.
    #[test]
    fn zip_hostile_inputs_are_clean() {
        let (_e, err) = decode_zip(&[0xAB; 4096]);
        assert!(err.is_some(), "non-zip garbage must error, not panic");
        let (_e2, err2) = decode_zip(&[]);
        assert!(err2.is_some(), "empty input must error, not panic");
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
