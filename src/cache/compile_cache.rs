//! WARM §2.2–2.5 — the content-addressed compile cache for `ascript run`.
//!
//! Key types:
//! - [`CompileCacheKey`]: everything that determines what bytes `ascript run <entry>` produces,
//!   serialized canonically and sha256'd to form the cache slot location.
//! - [`BinaryStamp`]: per-compiler-build invalidator (version + exe len + mtime).
//! - [`CacheManifest`]: per-slot validation data: all reachable modules with their
//!   per-file sha256 digests + the artifact's own sha256.
//! - [`validate_manifest`]: checks whether a previously-published slot is still valid.
//!
//! All paths go through fail-open error handling: any IO / parse failure → `cache-miss`
//! semantics, never a crash. The cache is an optimization layered over an unchanged
//! compile-and-run path.
//!
//! ## Key schema ("ck1")
//!
//! The schema tag `"ck1"` versions the KEY serialization algorithm, mirroring the
//! `asum1-` prefix convention in `src/pkg/hash.rs`. Rotate to `"ck2"` on any schema
//! change. The serialization is field-tagged and length-prefixed so every field is
//! self-delimiting and flags are flag-name-sorted (order-independent).

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─────────────────────────────────────────────────────────────────────────────
// BinaryStamp
// ─────────────────────────────────────────────────────────────────────────────

/// An invalidator that changes when the compiler binary is rebuilt, without hashing
/// the whole multi-MB executable every run.
///
/// Consists of:
/// - `CARGO_PKG_VERSION` — version string baked in at compile time.
/// - executable file length in bytes.
/// - executable file mtime as a 64-bit Unix timestamp (seconds since epoch).
///
/// Any rebuild rewrites the executable → new mtime / possibly new len → miss (correct
/// direction). A false *hit* would require an adversarially mtime-preserved,
/// same-length, different-content binary — not an accidental failure mode.
///
/// If we cannot stat `current_exe()`, we produce a [`BinaryStamp::Disabled`] sentinel.
/// A caller that sees `Disabled` should treat the cache as off (fail open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryStamp {
    /// Normal stamp: (version, exe_len, exe_mtime_secs).
    Valid {
        version: &'static str,
        exe_len: u64,
        exe_mtime_secs: u64,
    },
    /// Could not stat current_exe() — any error ⇒ cache-off.
    Disabled,
}

impl BinaryStamp {
    /// Compute the stamp for the running binary. Any error ⇒ [`BinaryStamp::Disabled`].
    pub fn current() -> Self {
        let path = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return BinaryStamp::Disabled,
        };
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return BinaryStamp::Disabled,
        };
        let exe_len = meta.len();
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => return BinaryStamp::Disabled,
        };
        let exe_mtime_secs = mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        BinaryStamp::Valid {
            version: env!("CARGO_PKG_VERSION"),
            exe_len,
            exe_mtime_secs,
        }
    }

    /// Returns `true` when the stamp is `Disabled` (caller should skip the cache).
    pub fn is_disabled(&self) -> bool {
        matches!(self, BinaryStamp::Disabled)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CompileCacheKey
// ─────────────────────────────────────────────────────────────────────────────

/// Everything that can change WHAT BYTES `ascript run <entry>` produces.
///
/// Serialized canonically (field-tagged, length-prefixed, flags sorted by flag name)
/// and sha256'd to form the cache **location key** (`ck1-<hex>`), mirroring the
/// `asum1-` algorithm-prefix convention from `src/pkg/hash.rs`.
///
/// Schema versioning: the `"ck1"` tag is baked into the serialized bytes; rotate to
/// `"ck2"` on any serialization change → old slots become unreachable garbage for
/// `ascript cache clean`.
///
/// Source identity is NOT embedded in the key — it is validated per-slot in the
/// [`CacheManifest`] (the two-level ccache direct-mode scheme, spec §2.5).
pub struct CompileCacheKey {
    /// Schema tag — `"ck1"`.
    pub key_schema: &'static str,
    /// `src/vm/aso.rs` `ASO_FORMAT_VERSION`.
    pub aso_format_version: u32,
    /// `src/vm/archive.rs` `ARCHIVE_VERSION`.
    pub archive_version: u16,
    /// Compiler-binary invalidator (spec §2.3).
    pub binary_stamp: BinaryStamp,
    /// Codegen-relevant flags as `(name, value)` pairs. MUST be sorted by name before
    /// serialization so flag order does not affect the key. v1 flags:
    ///   `[("debug","true"), ("shake","false")]`
    pub flags: Vec<(String, String)>,
    /// The **canonical** absolute entry path (path is part of the key — spec §2.4).
    pub entry_path: String,
    /// sha256 of the canonically-serialized resolved [`PackageMap`] (so a lockfile /
    /// resolution change → miss). All-zero when no package map.
    pub package_map_digest: [u8; 32],
}

impl CompileCacheKey {
    /// Serialize the key canonically (field-tagged, length-prefixed, flags sorted) and
    /// return the `ck1-<hex>` location key string.
    ///
    /// Canonical serialization: each field is written as `<tag_byte> <u32le_len> <payload>`,
    /// where the payload is UTF-8 for strings and LE bytes for integers. Flags are sorted
    /// by flag name BEFORE writing (the caller owns the sort; we sort defensively here too).
    pub fn location_key(&self) -> String {
        let bytes = self.serialize();
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("ck1-{hex}")
    }

    /// Canonical byte serialization of the key (the bytes that are sha256'd).
    ///
    /// Format per field:
    /// ```text
    /// 0x01 · u32le(len(key_schema))  · key_schema
    /// 0x02 · u32le(4)                · aso_format_version as u32le
    /// 0x03 · u32le(2)                · archive_version as u16le
    /// 0x04 · <binary_stamp_bytes>    (tag-prefixed sub-blob)
    /// 0x05 · u32le(flags_blob_len)   · sorted_flags_blob
    ///         (each flag: u32le(name_len)·name · u32le(val_len)·val)
    /// 0x06 · u32le(len(entry_path))  · entry_path
    /// 0x07 · u32le(32)               · package_map_digest
    /// ```
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);

        // Field 0x01: key_schema
        write_tagged_str(&mut out, 0x01, self.key_schema);

        // Field 0x02: aso_format_version (u32le)
        write_tag_and_len(&mut out, 0x02, 4);
        out.extend_from_slice(&self.aso_format_version.to_le_bytes());

        // Field 0x03: archive_version (u16le)
        write_tag_and_len(&mut out, 0x03, 2);
        out.extend_from_slice(&self.archive_version.to_le_bytes());

        // Field 0x04: binary_stamp (tagged sub-blob)
        match &self.binary_stamp {
            BinaryStamp::Disabled => {
                // A single byte sentinel: 0x00
                write_tag_and_len(&mut out, 0x04, 1);
                out.push(0x00);
            }
            BinaryStamp::Valid {
                version,
                exe_len,
                exe_mtime_secs,
            } => {
                // Sub-blob: 0x01 · u32le(ver_len) · ver · u64le(len) · u64le(mtime)
                let ver_bytes = version.as_bytes();
                let sub_len = 1 + 4 + ver_bytes.len() + 8 + 8;
                write_tag_and_len(&mut out, 0x04, sub_len);
                out.push(0x01);
                out.extend_from_slice(&(ver_bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(ver_bytes);
                out.extend_from_slice(&exe_len.to_le_bytes());
                out.extend_from_slice(&exe_mtime_secs.to_le_bytes());
            }
        }

        // Field 0x05: flags (sorted by name)
        let mut flags = self.flags.clone();
        flags.sort_by(|a, b| a.0.cmp(&b.0));
        let mut flags_blob: Vec<u8> = Vec::new();
        for (name, value) in &flags {
            let nb = name.as_bytes();
            let vb = value.as_bytes();
            flags_blob.extend_from_slice(&(nb.len() as u32).to_le_bytes());
            flags_blob.extend_from_slice(nb);
            flags_blob.extend_from_slice(&(vb.len() as u32).to_le_bytes());
            flags_blob.extend_from_slice(vb);
        }
        write_tag_and_len(&mut out, 0x05, flags_blob.len());
        out.extend_from_slice(&flags_blob);

        // Field 0x06: entry_path
        write_tagged_str(&mut out, 0x06, &self.entry_path);

        // Field 0x07: package_map_digest (32 bytes)
        write_tag_and_len(&mut out, 0x07, 32);
        out.extend_from_slice(&self.package_map_digest);

        out
    }
}

fn write_tag_and_len(out: &mut Vec<u8>, tag: u8, len: usize) {
    out.push(tag);
    out.extend_from_slice(&(len as u32).to_le_bytes());
}

fn write_tagged_str(out: &mut Vec<u8>, tag: u8, s: &str) {
    let b = s.as_bytes();
    write_tag_and_len(out, tag, b.len());
    out.extend_from_slice(b);
}

// ─────────────────────────────────────────────────────────────────────────────
// package_map_digest
// ─────────────────────────────────────────────────────────────────────────────

/// Compute a sha256 digest over a canonically-serialized [`PackageMap`].
///
/// Canonical form: all entries sorted by key (so insertion order doesn't matter),
/// each entry written as `u32le(key_len)·key·u32le(root_len)·root·u32le(entry_len)·entry`.
/// An empty map produces a zero digest (all-zero sentinel, not a sha256 of "").
pub fn package_map_digest(packages: &crate::interp::PackageMap) -> [u8; 32] {
    if packages.is_empty() {
        return [0u8; 32];
    }
    let mut pairs: Vec<(&String, &crate::interp::ResolvedPkg)> = packages.iter().collect();
    pairs.sort_by_key(|(k, _)| *k);

    let mut h = Sha256::new();
    for (key, pkg) in &pairs {
        let kb = key.as_bytes();
        h.update((kb.len() as u32).to_le_bytes());
        h.update(kb);

        let root = pkg.root.to_string_lossy();
        let rb = root.as_bytes();
        h.update((rb.len() as u32).to_le_bytes());
        h.update(rb);

        let entry = pkg.entry.to_string_lossy();
        let eb = entry.as_bytes();
        h.update((eb.len() as u32).to_le_bytes());
        h.update(eb);
    }
    h.finalize().into()
}

// ─────────────────────────────────────────────────────────────────────────────
// CacheManifest (the per-slot validation data)
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in the cache manifest: a reachable module with its sha256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestModule {
    /// Archive-style logical key (the `join_logical` convention).
    pub logical_key: String,
    /// Absolute on-disk path to the source file.
    pub path: PathBuf,
    /// sha256 of the source file bytes at publish time.
    pub sha256: [u8; 32],
}

/// The per-cache-slot manifest: the reachable module set + artifact digest.
///
/// A **hit** requires every listed module to re-hash byte-for-byte equal AND
/// the artifact to re-hash equal. Any mismatch → miss → recompile.
///
/// JSON wire form (hand-rolled — no serde_json dep at the cache layer):
/// ```json
/// {
///   "modules": [
///     {"logical_key":"main.as","path":"/abs/path","sha256":"<hex>"}
///   ],
///   "artifact_sha256": "<hex>",
///   "created_unix_ms": 1234567890000
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheManifest {
    pub modules: Vec<ManifestModule>,
    pub artifact_sha256: [u8; 32],
    pub created_unix_ms: u64,
}

impl CacheManifest {
    /// Serialize to the JSON wire form (hand-rolled; no external dep required).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n  \"modules\": [\n");
        for (i, m) in self.modules.iter().enumerate() {
            out.push_str("    {\"logical_key\":");
            push_json_str(&mut out, &m.logical_key);
            out.push_str(",\"path\":");
            push_json_str(&mut out, &m.path.to_string_lossy());
            out.push_str(",\"sha256\":");
            push_json_str(&mut out, &hex_digest(&m.sha256));
            out.push('}');
            if i + 1 < self.modules.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n  \"artifact_sha256\":");
        push_json_str(&mut out, &hex_digest(&self.artifact_sha256));
        out.push_str(",\n  \"created_unix_ms\":");
        out.push_str(&self.created_unix_ms.to_string());
        out.push_str("\n}\n");
        out
    }

    /// Parse from the JSON wire form. Returns `None` on any parse error — the
    /// caller treats that as a cache miss (fail open).
    pub fn from_json(s: &str) -> Option<Self> {
        parse_manifest(s)
    }
}

/// Hex-encode a 32-byte digest.
fn hex_digest(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a hex-encoded 32-byte digest. Returns `None` on any format error.
fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Escape a string for JSON (minimal: handles `\`, `"`, and control characters).
fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ─────────────────────────────────────────────────────────────────────────────
// Hand-rolled JSON parser for CacheManifest
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal, robust JSON parser for [`CacheManifest::from_json`].
/// Returns `None` on any anomaly — the caller treats that as a cache miss.
fn parse_manifest(s: &str) -> Option<CacheManifest> {
    let s = s.trim();
    let s = s.strip_prefix('{')?.trim_start();
    let s = s.strip_suffix('}').map(str::trim_end)?;

    // Pull out named top-level fields.  We accept them in any order.
    let mut modules: Option<Vec<ManifestModule>> = None;
    let mut artifact_sha256: Option<[u8; 32]> = None;
    let mut created_unix_ms: Option<u64> = None;

    let mut rest = s.trim();
    while !rest.is_empty() {
        // field key
        if rest.starts_with('"') {
            let (key, after_key) = parse_json_string(rest)?;
            let after_key = after_key.trim_start();
            let after_colon = after_key.strip_prefix(':')?.trim_start();

            match key.as_str() {
                "modules" => {
                    let (mods, tail) = parse_modules_array(after_colon)?;
                    modules = Some(mods);
                    rest = tail.trim_start();
                }
                "artifact_sha256" => {
                    let (val, tail) = parse_json_string(after_colon)?;
                    artifact_sha256 = Some(parse_hex32(&val)?);
                    rest = tail.trim_start();
                }
                "created_unix_ms" => {
                    let (val, tail) = parse_json_number(after_colon)?;
                    created_unix_ms = Some(val);
                    rest = tail.trim_start();
                }
                _ => {
                    // unknown field: skip the value
                    let (_, tail) = skip_json_value(after_colon)?;
                    rest = tail.trim_start();
                }
            }
        } else {
            break;
        }

        if rest.starts_with(',') {
            rest = rest[1..].trim_start();
        }
    }

    Some(CacheManifest {
        modules: modules?,
        artifact_sha256: artifact_sha256?,
        created_unix_ms: created_unix_ms?,
    })
}

fn parse_modules_array(s: &str) -> Option<(Vec<ManifestModule>, &str)> {
    let s = s.strip_prefix('[')?.trim_start();
    let mut mods = Vec::new();
    let mut rest = s;

    loop {
        let rest_trimmed = rest.trim_start();
        if let Some(after_bracket) = rest_trimmed.strip_prefix(']') {
            return Some((mods, after_bracket));
        }
        if mods.is_empty() && rest_trimmed.is_empty() {
            return None;
        }
        if !mods.is_empty() {
            let without_comma = rest_trimmed.strip_prefix(',')?.trim_start();
            if let Some(after_bracket) = without_comma.strip_prefix(']') {
                return Some((mods, after_bracket));
            }
            rest = without_comma;
        } else {
            rest = rest_trimmed;
        }

        // Parse one module object.
        let (m, tail) = parse_module_obj(rest)?;
        mods.push(m);
        rest = tail.trim_start();
    }
}

fn parse_module_obj(s: &str) -> Option<(ManifestModule, &str)> {
    let s = s.strip_prefix('{')?.trim_start();
    let mut logical_key: Option<String> = None;
    let mut path: Option<PathBuf> = None;
    let mut sha256: Option<[u8; 32]> = None;

    let mut rest = s;
    loop {
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.starts_with('}') {
            break;
        }
        // field
        if !rest_trimmed.starts_with('"') {
            return None;
        }
        let (key, after_key) = parse_json_string(rest_trimmed)?;
        let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
        match key.as_str() {
            "logical_key" => {
                let (val, tail) = parse_json_string(after_colon)?;
                logical_key = Some(val);
                rest = tail;
            }
            "path" => {
                let (val, tail) = parse_json_string(after_colon)?;
                path = Some(PathBuf::from(val));
                rest = tail;
            }
            "sha256" => {
                let (val, tail) = parse_json_string(after_colon)?;
                sha256 = Some(parse_hex32(&val)?);
                rest = tail;
            }
            _ => {
                let (_, tail) = skip_json_value(after_colon)?;
                rest = tail;
            }
        }
        rest = rest.trim_start();
        if rest.starts_with(',') {
            rest = &rest[1..];
        }
    }
    let after_brace = rest.trim_start().strip_prefix('}')?;
    Some((
        ManifestModule {
            logical_key: logical_key?,
            path: path?,
            sha256: sha256?,
        },
        after_brace,
    ))
}

/// Parse a JSON string literal starting with `"`. Returns `(value, rest_after_closing_quote)`.
fn parse_json_string(s: &str) -> Option<(String, &str)> {
    let s = s.strip_prefix('"')?;
    let mut result = String::new();
    let mut chars = s.char_indices();
    loop {
        let (i, c) = chars.next()?;
        match c {
            '"' => {
                return Some((result, &s[i + 1..]));
            }
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    '/' => result.push('/'),
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    'u' => {
                        // \uXXXX
                        let mut hex = String::new();
                        for _ in 0..4 {
                            let (_, hc) = chars.next()?;
                            hex.push(hc);
                        }
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        result.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            c => result.push(c),
        }
    }
}

/// Parse a JSON unsigned integer. Returns `(value, rest)`.
fn parse_json_number(s: &str) -> Option<(u64, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let n = s[..end].parse().ok()?;
    Some((n, &s[end..]))
}

/// Skip any JSON value (string, number, array, object, true/false/null).
/// Returns `((), rest_after_value)`.
fn skip_json_value(s: &str) -> Option<((), &str)> {
    let s = s.trim_start();
    if s.starts_with('"') {
        let (_, rest) = parse_json_string(s)?;
        return Some(((), rest));
    }
    if s.starts_with('[') {
        let mut depth = 0usize;
        let mut in_str = false;
        let mut escape = false;
        for (i, c) in s.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if in_str {
                if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '[' | '{' => depth += 1,
                ']' | '}' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Some(((), &s[i + c.len_utf8()..]));
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if s.starts_with('{') {
        let mut depth = 0usize;
        let mut in_str = false;
        let mut escape = false;
        for (i, c) in s.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if in_str {
                if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '[' | '{' => depth += 1,
                ']' | '}' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Some(((), &s[i + c.len_utf8()..]));
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    // number / true / false / null
    let end = s
        .find(|c: char| matches!(c, ',' | '}' | ']') || c.is_whitespace())
        .unwrap_or(s.len());
    Some(((), &s[end..]))
}

// ─────────────────────────────────────────────────────────────────────────────
// validate_manifest
// ─────────────────────────────────────────────────────────────────────────────

/// Result of [`validate_manifest`].
#[derive(Debug, PartialEq, Eq)]
pub enum ValidateResult {
    /// Every listed file re-hashes equal AND the artifact digest matches → cache hit.
    Hit,
    /// One or more files changed, or the artifact is corrupt → cache miss.
    Miss,
}

/// Validate a previously-published cache slot.
///
/// **Hit conditions** (ALL must hold):
/// 1. Every module listed in `manifest.modules` exists on disk and its current
///    sha256 matches the stored digest.
/// 2. The artifact bytes `artifact_bytes` sha256-hash to `manifest.artifact_sha256`.
///
/// Any anomaly (missing file, IO error, digest mismatch) → [`ValidateResult::Miss`].
/// Extra files NOT listed in the manifest have no effect (they cannot change the
/// compiled output if the listed files are unchanged — spec §2.5 soundness argument).
pub fn validate_manifest(manifest: &CacheManifest, artifact_bytes: &[u8]) -> ValidateResult {
    // Check artifact digest first — cheap and catches corruption.
    let art_digest: [u8; 32] = Sha256::digest(artifact_bytes).into();
    if art_digest != manifest.artifact_sha256 {
        return ValidateResult::Miss;
    }

    // Check every listed source file.
    for m in &manifest.modules {
        let current_sha256 = match sha256_file(&m.path) {
            Ok(d) => d,
            Err(_) => return ValidateResult::Miss,
        };
        if current_sha256 != m.sha256 {
            return ValidateResult::Miss;
        }
    }

    ValidateResult::Hit
}

/// Compute the sha256 of a file. Returns `Err` on any IO error.
pub fn sha256_file(path: &Path) -> Result<[u8; 32], std::io::Error> {
    let bytes = std::fs::read(path)?;
    Ok(Sha256::digest(&bytes).into())
}

/// Compute the sha256 of in-memory bytes.
pub fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Compute the current Unix timestamp in milliseconds (for `created_unix_ms`).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ascript-cache-test-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_key(entry: &str, flags: Vec<(&str, &str)>, pkg_digest: [u8; 32]) -> CompileCacheKey {
        CompileCacheKey {
            key_schema: "ck1",
            aso_format_version: 29,
            archive_version: 1,
            binary_stamp: BinaryStamp::Valid {
                version: "1.0.0",
                exe_len: 123456,
                exe_mtime_secs: 1700000000,
            },
            flags: flags
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            entry_path: entry.to_string(),
            package_map_digest: pkg_digest,
        }
    }

    fn sample_manifest(dir: &Path) -> (CacheManifest, Vec<u8>) {
        // Write two source files so we can hash them.
        let main_path = dir.join("main.as");
        let util_path = dir.join("util.as");
        fs::write(&main_path, b"import './util'\nprint('hello')").unwrap();
        fs::write(&util_path, b"fn greet() { 'hi' }").unwrap();

        let main_digest = sha256_file(&main_path).unwrap();
        let util_digest = sha256_file(&util_path).unwrap();

        let artifact_bytes = b"fake-aso-bytes";
        let art_digest = sha256_bytes(artifact_bytes);

        let manifest = CacheManifest {
            modules: vec![
                ManifestModule {
                    logical_key: "main.as".to_string(),
                    path: main_path,
                    sha256: main_digest,
                },
                ManifestModule {
                    logical_key: "util.as".to_string(),
                    path: util_path,
                    sha256: util_digest,
                },
            ],
            artifact_sha256: art_digest,
            created_unix_ms: 1_700_000_000_000,
        };
        (manifest, artifact_bytes.to_vec())
    }

    // ── CompileCacheKey tests ─────────────────────────────────────────────────

    /// Flag ORDER must be irrelevant: two keys differing only in flag order must collide.
    #[test]
    fn flag_order_irrelevant_collision() {
        let key_ab = make_key(
            "/entry.as",
            vec![("debug", "true"), ("shake", "false")],
            [0u8; 32],
        );
        let key_ba = make_key(
            "/entry.as",
            vec![("shake", "false"), ("debug", "true")],
            [0u8; 32],
        );
        assert_eq!(
            key_ab.location_key(),
            key_ba.location_key(),
            "flag order must not affect the cache key"
        );
    }

    /// The schema tag `ck1` must appear in the serialized bytes.
    #[test]
    fn schema_tag_present_in_serialized_form() {
        let key = make_key("/e.as", vec![], [0u8; 32]);
        let bytes = key.serialize();
        assert!(
            bytes
                .windows(3)
                .any(|w| w == b"ck1"),
            "serialized bytes must contain the schema tag 'ck1'"
        );
    }

    /// The location key must be `ck1-` prefixed.
    #[test]
    fn location_key_has_ck1_prefix() {
        let key = make_key("/e.as", vec![], [0u8; 32]);
        let lk = key.location_key();
        assert!(lk.starts_with("ck1-"), "location key must start with 'ck1-', got {lk}");
        // The hex suffix is 64 characters (32 bytes).
        assert_eq!(lk.len(), 4 + 64, "location key must be ck1-<64hex>");
    }

    /// Every field perturbation must produce a DISTINCT location key.
    #[test]
    fn distinct_on_each_field_perturbation() {
        let base = make_key("/entry.as", vec![("debug", "true")], [0u8; 32]);
        let base_key = base.location_key();

        // Different entry path.
        let k1 = make_key("/other.as", vec![("debug", "true")], [0u8; 32]);
        assert_ne!(base_key, k1.location_key(), "entry path perturbation must change key");

        // Different flag value.
        let k2 = make_key("/entry.as", vec![("debug", "false")], [0u8; 32]);
        assert_ne!(base_key, k2.location_key(), "flag value perturbation must change key");

        // Different flag name.
        let k3 = make_key("/entry.as", vec![("shake", "true")], [0u8; 32]);
        assert_ne!(base_key, k3.location_key(), "flag name perturbation must change key");

        // Different package_map_digest.
        let mut pkg_digest = [0u8; 32];
        pkg_digest[0] = 1;
        let k4 = make_key("/entry.as", vec![("debug", "true")], pkg_digest);
        assert_ne!(base_key, k4.location_key(), "package_map_digest perturbation must change key");

        // Different ASO format version.
        let k5 = CompileCacheKey {
            aso_format_version: 99,
            ..make_key("/entry.as", vec![("debug", "true")], [0u8; 32])
        };
        assert_ne!(base_key, k5.location_key(), "aso_format_version perturbation must change key");

        // Different archive version.
        let k6 = CompileCacheKey {
            archive_version: 2,
            ..make_key("/entry.as", vec![("debug", "true")], [0u8; 32])
        };
        assert_ne!(base_key, k6.location_key(), "archive_version perturbation must change key");

        // Different binary stamp.
        let k7 = CompileCacheKey {
            binary_stamp: BinaryStamp::Valid {
                version: "9.9.9",
                exe_len: 999,
                exe_mtime_secs: 999,
            },
            ..make_key("/entry.as", vec![("debug", "true")], [0u8; 32])
        };
        assert_ne!(base_key, k7.location_key(), "binary_stamp perturbation must change key");
    }

    // ── CacheManifest round-trip ──────────────────────────────────────────────

    #[test]
    fn manifest_json_round_trip() {
        let dir = scratch_dir("json_rt");
        let (manifest, _) = sample_manifest(&dir);
        let json = manifest.to_json();
        let parsed = CacheManifest::from_json(&json).expect("manifest must parse");
        assert_eq!(manifest, parsed, "manifest round-trip must be lossless");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manifest_round_trip_special_chars_in_paths() {
        // Paths with backslashes and quotes in the JSON-escaped form.
        let manifest = CacheManifest {
            modules: vec![ManifestModule {
                logical_key: r#"mod with "quotes".as"#.to_string(),
                path: PathBuf::from(r#"/tmp/dir with "spaces"/file.as"#),
                sha256: [0xAB; 32],
            }],
            artifact_sha256: [0xCD; 32],
            created_unix_ms: 42,
        };
        let json = manifest.to_json();
        let parsed = CacheManifest::from_json(&json).expect("must parse");
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn manifest_garbage_json_returns_none() {
        assert!(CacheManifest::from_json("not json at all").is_none());
        assert!(CacheManifest::from_json("").is_none());
        assert!(CacheManifest::from_json("{}").is_none()); // missing required fields
        assert!(CacheManifest::from_json("{\"modules\":[],\"artifact_sha256\":\"zzz\",\"created_unix_ms\":0}").is_none());
    }

    // ── validate_manifest ─────────────────────────────────────────────────────

    /// validate_manifest returns Hit when files are unchanged and artifact matches.
    #[test]
    fn validate_hit_when_unchanged() {
        let dir = scratch_dir("hit");
        let (manifest, artifact) = sample_manifest(&dir);
        assert_eq!(
            validate_manifest(&manifest, &artifact),
            ValidateResult::Hit
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Editing any listed file ⇒ Miss.
    #[test]
    fn validate_miss_on_edited_file() {
        let dir = scratch_dir("edit");
        let (manifest, artifact) = sample_manifest(&dir);
        // Edit the first listed file.
        let path = &manifest.modules[0].path;
        fs::write(path, b"// edited").unwrap();
        assert_eq!(
            validate_manifest(&manifest, &artifact),
            ValidateResult::Miss
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Deleting any listed file ⇒ Miss.
    #[test]
    fn validate_miss_on_deleted_file() {
        let dir = scratch_dir("del");
        let (manifest, artifact) = sample_manifest(&dir);
        let path = manifest.modules[1].path.clone();
        fs::remove_file(&path).unwrap();
        assert_eq!(
            validate_manifest(&manifest, &artifact),
            ValidateResult::Miss
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Touching a file (changing its mtime without changing content) ⇒ still Hit.
    /// The cache validates via content digests, NOT mtimes.
    #[test]
    fn validate_hit_on_mtime_only_touch() {
        let dir = scratch_dir("mtime");
        let (manifest, artifact) = sample_manifest(&dir);
        // Re-write the exact same content (new mtime, same bytes).
        let path = &manifest.modules[0].path;
        let content = fs::read(path).unwrap();
        fs::write(path, &content).unwrap();
        assert_eq!(
            validate_manifest(&manifest, &artifact),
            ValidateResult::Hit,
            "mtime-only touch must still be a Hit"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// An extra file NOT listed in the manifest ⇒ still Hit.
    #[test]
    fn validate_hit_with_extra_unrelated_file() {
        let dir = scratch_dir("extra");
        let (manifest, artifact) = sample_manifest(&dir);
        fs::write(dir.join("extra.as"), b"// unrelated").unwrap();
        assert_eq!(
            validate_manifest(&manifest, &artifact),
            ValidateResult::Hit,
            "extra unlisted file must not affect validation"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// A bit-flipped artifact ⇒ Miss.
    #[test]
    fn validate_miss_on_corrupt_artifact() {
        let dir = scratch_dir("corrupt");
        let (manifest, artifact) = sample_manifest(&dir);
        let mut bad_artifact = artifact.clone();
        if !bad_artifact.is_empty() {
            bad_artifact[0] ^= 0xFF;
        }
        assert_eq!(
            validate_manifest(&manifest, &bad_artifact),
            ValidateResult::Miss
        );
        let _ = fs::remove_dir_all(dir);
    }

    // ── package_map_digest ────────────────────────────────────────────────────

    #[test]
    fn package_map_digest_empty_is_all_zero() {
        use std::collections::HashMap;
        let empty: crate::interp::PackageMap = HashMap::new();
        assert_eq!(package_map_digest(&empty), [0u8; 32]);
    }

    #[test]
    fn package_map_digest_order_independent() {
        use crate::interp::{PackageMap, ResolvedPkg};
        use std::collections::HashMap;

        fn make_pkg(root: &str, entry: &str) -> ResolvedPkg {
            ResolvedPkg {
                root: PathBuf::from(root),
                entry: PathBuf::from(entry),
            }
        }

        let mut m1: PackageMap = HashMap::new();
        m1.insert("http".to_string(), make_pkg("/store/asum1-abc/", "/store/asum1-abc/http/index.as"));
        m1.insert("json".to_string(), make_pkg("/store/asum1-def/", "/store/asum1-def/json/index.as"));

        let mut m2: PackageMap = HashMap::new();
        m2.insert("json".to_string(), make_pkg("/store/asum1-def/", "/store/asum1-def/json/index.as"));
        m2.insert("http".to_string(), make_pkg("/store/asum1-abc/", "/store/asum1-abc/http/index.as"));

        assert_eq!(
            package_map_digest(&m1),
            package_map_digest(&m2),
            "package_map_digest must be order-independent"
        );
    }

    // ── BinaryStamp ───────────────────────────────────────────────────────────

    #[test]
    fn binary_stamp_current_succeeds() {
        // In a test environment current_exe() usually works.
        let s = BinaryStamp::current();
        // We can't assert Valid vs Disabled here because CI might have unusual setups,
        // but we can assert it doesn't panic and is one of the two variants.
        match &s {
            BinaryStamp::Valid { version, .. } => {
                assert!(!version.is_empty());
            }
            BinaryStamp::Disabled => {}
        }
    }

    #[test]
    fn binary_stamp_disabled_marks_cache_off() {
        assert!(BinaryStamp::Disabled.is_disabled());
        assert!(!BinaryStamp::Valid {
            version: "1.0",
            exe_len: 1,
            exe_mtime_secs: 1
        }
        .is_disabled());
    }

    #[test]
    fn binary_stamp_different_version_gives_different_key() {
        let k1 = CompileCacheKey {
            binary_stamp: BinaryStamp::Valid {
                version: "1.0.0",
                exe_len: 100,
                exe_mtime_secs: 1000,
            },
            ..make_key("/e.as", vec![], [0u8; 32])
        };
        let k2 = CompileCacheKey {
            binary_stamp: BinaryStamp::Valid {
                version: "1.0.1",
                exe_len: 100,
                exe_mtime_secs: 1000,
            },
            ..make_key("/e.as", vec![], [0u8; 32])
        };
        assert_ne!(k1.location_key(), k2.location_key());
    }
}
