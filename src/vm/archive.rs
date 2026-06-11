//! ARCHIVE — the module-archive container codec (self-contained-bundles spec §3).
//!
//! A [`ModuleArchive`] bundles a program's whole reachable module graph plus a small
//! manifest (entry-module index, an embedded [`CapSet`], a tree-shake report digest) so the
//! program runs with NO source tree on disk. Both `ascript build` and `--native` produce one;
//! later phases build it from the import graph, consult it at runtime, and ship it to workers.
//!
//! This module is the CONTAINER CODEC only: [`ModuleArchive::encode`] /
//! [`ModuleArchive::decode`]. Per-module chunk bytes are stored OPAQUE — a verified `.aso`
//! chunk is carried through unmodified and re-verified lazily on load (via
//! `Chunk::from_bytes_verified` in a later task), never here.
//!
//! [`ModuleArchive::decode`] parses UNTRUSTED bytes (a tampered archive), so — like the
//! sibling [`CapSet::from_bytes`] and the `.aso` reader — it NEVER panics: every length read is
//! bounds-checked against the remaining input and every count/length is capped before any
//! allocation (allocation-bomb safe).

use crate::stdlib::caps::{CapSet, CapsDecodeError};

/// The archive container magic — DISTINCT from `BUNDLE_MAGIC` (`b"ASCRIPTB"`) and `ASO_MAGIC`
/// (`b"ASO\0"`) so a bare `.aso`, a native bundle, and a module archive are all separable by a
/// leading-bytes magic dispatch (enabling later magic-routing on load).
pub const ARCHIVE_MAGIC: [u8; 8] = *b"ASCRIPTA";

/// The archive container layout version (bump on ANY change to the archive framing —
/// independent of `ASO_FORMAT_VERSION`, which versions the embedded per-module chunks).
pub const ARCHIVE_VERSION: u16 = 1;

/// A module archive: a manifest plus the reachable module graph as `(key, opaque .aso bytes)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleArchive {
    /// Index into [`modules`](Self::modules) of the entry module.
    pub entry: u32,
    /// The embedded capability set (encoded via the Task 1.1 [`CapSet`] codec).
    pub caps: CapSet,
    /// The tree-shake report digest (filled by Phase 2; all-zero until then).
    pub shake_digest: [u8; 32],
    /// Per module: its logical path key and the verified `.aso` chunk bytes (stored opaque).
    pub modules: Vec<(String, Vec<u8>)>,
}

/// Cap on the module count and on each key/chunk length, applied BEFORE any `Vec::with_capacity`
/// / `reserve`, so a tampered length can never drive a multi-gigabyte allocation. The real count
/// still drives the decode loop; a short read then surfaces as a clean [`ArchiveError::Truncated`].
///
/// A real program graph is O(hundreds) of modules; `module_count` is also further clamped by the
/// reader's `remaining()` bytes (every module needs ≥ 8 framing bytes), so this is a generous
/// secondary ceiling.
const MAX_MODULES: usize = 1 << 20;

/// An error decoding a [`ModuleArchive`] from (possibly hostile) bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// The leading 8 bytes were not [`ARCHIVE_MAGIC`].
    BadMagic([u8; 8]),
    /// The container version did not match [`ARCHIVE_VERSION`].
    VersionMismatch { got: u16, expected: u16 },
    /// The input ended before a required field could be read.
    Truncated,
    /// A length or count read does not fit in the host `usize`.
    Overflow,
    /// The declared module count exceeded [`MAX_MODULES`].
    TooManyModules(usize),
    /// A module's logical-path key was not valid UTF-8.
    InvalidUtf8,
    /// The entry index does not point at a real module (`entry >= module_count`).
    EntryOutOfRange { entry: u32, count: usize },
    /// The embedded [`CapSet`] failed to decode.
    Caps(CapsDecodeError),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::BadMagic(got) => write!(
                f,
                "not a module archive (bad magic {got:?}, expected {ARCHIVE_MAGIC:?})"
            ),
            ArchiveError::VersionMismatch { got, expected } => write!(
                f,
                "unsupported module-archive version {got} (this runtime expects {expected})"
            ),
            ArchiveError::Truncated => write!(f, "truncated module archive"),
            ArchiveError::Overflow => write!(f, "module-archive length field overflows usize"),
            ArchiveError::TooManyModules(n) => {
                write!(f, "module count {n} exceeds the maximum {MAX_MODULES}")
            }
            ArchiveError::InvalidUtf8 => write!(f, "module key is not valid UTF-8"),
            ArchiveError::EntryOutOfRange { entry, count } => write!(
                f,
                "entry index {entry} is out of range for {count} module(s)"
            ),
            ArchiveError::Caps(e) => write!(f, "embedded capability set: {e}"),
        }
    }
}

impl std::error::Error for ArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ArchiveError::Caps(e) => Some(e),
            _ => None,
        }
    }
}

impl From<CapsDecodeError> for ArchiveError {
    fn from(e: CapsDecodeError) -> Self {
        ArchiveError::Caps(e)
    }
}

/// A bounds-checked forward little-endian reader. Every accessor advances `pos` only after the
/// read is proven to fit, so an out-of-range read is a clean [`ArchiveError::Truncated`], never a
/// slice panic.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ArchiveError> {
        let end = self.pos.checked_add(n).ok_or(ArchiveError::Overflow)?;
        let slice = self.buf.get(self.pos..end).ok_or(ArchiveError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
    /// Read a fixed-size `[u8; N]` array — a clean `Truncated` on a short read, NO panic
    /// (`take(N)` returns exactly `N` bytes or `Err`, so the length-checked `copy_from_slice`
    /// cannot panic). Keeps the decode path literally `.expect()`-free.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], ArchiveError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u16(&mut self) -> Result<u16, ArchiveError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, ArchiveError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// A `u32` length narrowed to `usize`.
    fn len(&mut self) -> Result<usize, ArchiveError> {
        usize::try_from(self.u32()?).map_err(|_| ArchiveError::Overflow)
    }
    /// Bytes left unread — the hard ceiling on any length-driven pre-allocation (every element
    /// is ≥ 1 byte, so a declared count above this cannot be satisfied).
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    /// A `u32`-length-prefixed byte run (the prefix is bounds-checked by `take`).
    fn bytes(&mut self) -> Result<&'a [u8], ArchiveError> {
        let n = self.len()?;
        self.take(n)
    }
    /// A `u32`-length-prefixed UTF-8 string.
    fn str(&mut self) -> Result<String, ArchiveError> {
        let b = self.bytes()?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| ArchiveError::InvalidUtf8)
    }
}

/// A `u32` length prefix written little-endian. Lengths here are bounded by real module-graph
/// sizes (a chunk that exceeds `u32::MAX` is already rejected by the `.aso` writer that produced
/// it), so a plain saturating cast is sufficient — `encode` is over our OWN data, not hostile
/// bytes.
fn write_len(out: &mut Vec<u8>, n: usize) {
    let v = u32::try_from(n).unwrap_or(u32::MAX);
    out.extend_from_slice(&v.to_le_bytes());
}

impl ModuleArchive {
    /// Serialize to the archive wire form:
    ///
    /// ```text
    /// magic(8) · version(u16) · entry(u32) · caps_len(u32) · caps(caps_len)
    ///          · shake_digest(32) · module_count(u32)
    ///          · [ key_len(u32) · key(utf8) · chunk_len(u32) · chunk(bytes) ] × count
    /// ```
    ///
    /// The embedded `CapSet` is length-prefixed (via its own [`CapSet::to_bytes`] codec) so
    /// `decode` knows exactly where it ends without relying on its trailing-data tolerance.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&ARCHIVE_MAGIC);
        out.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.entry.to_le_bytes());

        let caps = self.caps.to_bytes();
        write_len(&mut out, caps.len());
        out.extend_from_slice(&caps);

        out.extend_from_slice(&self.shake_digest);

        write_len(&mut out, self.modules.len());
        for (key, chunk) in &self.modules {
            write_len(&mut out, key.len());
            out.extend_from_slice(key.as_bytes());
            write_len(&mut out, chunk.len());
            out.extend_from_slice(chunk);
        }
        out
    }

    /// Parse a [`ModuleArchive`] from `b`. `b` is UNTRUSTED: every read is bounds-checked and
    /// every count/length is capped before allocation, so a malformed or hostile archive yields
    /// an [`ArchiveError`], NEVER a panic.
    pub fn decode(b: &[u8]) -> Result<ModuleArchive, ArchiveError> {
        let mut r = Reader::new(b);

        let magic: [u8; 8] = r.take_array()?;
        if magic != ARCHIVE_MAGIC {
            return Err(ArchiveError::BadMagic(magic));
        }

        let version = r.u16()?;
        if version != ARCHIVE_VERSION {
            return Err(ArchiveError::VersionMismatch {
                got: version,
                expected: ARCHIVE_VERSION,
            });
        }

        let entry = r.u32()?;

        // The embedded CapSet is its own length-prefixed sub-blob; decode it from exactly that
        // slice so any of its own trailing-tolerance can't swallow archive bytes.
        let caps_bytes = r.bytes()?;
        let (caps, _consumed) = CapSet::from_bytes(caps_bytes)?;

        let shake_digest: [u8; 32] = r.take_array()?;

        let module_count = r.len()?;
        if module_count > MAX_MODULES {
            return Err(ArchiveError::TooManyModules(module_count));
        }
        // The entry must point at a real module.
        if (entry as usize) >= module_count {
            return Err(ArchiveError::EntryOutOfRange {
                entry,
                count: module_count,
            });
        }

        // Clamp the pre-allocation to the bytes that could possibly remain (each module needs at
        // least its two 4-byte length prefixes); the real count still drives the loop, and a short
        // read surfaces as a clean `Truncated`.
        let mut modules = Vec::with_capacity(module_count.min(r.remaining()));
        for _ in 0..module_count {
            let key = r.str()?;
            let chunk = r.bytes()?.to_vec();
            modules.push((key, chunk));
        }

        Ok(ModuleArchive {
            entry,
            caps,
            shake_digest,
            modules,
        })
    }

    /// Look up a module's opaque `.aso` chunk bytes by its logical key.
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.modules
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, chunk)| chunk.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::BUNDLE_MAGIC;
    use crate::vm::aso::ASO_MAGIC;

    /// A non-trivial CapSet for the round-trip fixtures: a real deny carve-out so the embedded
    /// codec carries more than the default bits byte.
    fn sample_caps() -> CapSet {
        // `from_deny_list` produces a CapSet with dropped caps; using the names keeps this robust
        // to the bitset layout.
        CapSet::from_deny_list(["net", "process"]).expect("valid deny list")
    }

    fn sample_archive() -> ModuleArchive {
        let mut shake = [0u8; 32];
        for (i, byte) in shake.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(7).wrapping_add(1);
        }
        ModuleArchive {
            entry: 0,
            caps: sample_caps(),
            shake_digest: shake,
            modules: vec![
                ("main".to_string(), vec![0xAA, 0xBB, 0xCC, 0x00, 0xFF]),
                ("std/foo".to_string(), vec![1, 2, 3]),
                ("pkg/bar/baz".to_string(), vec![]),
            ],
        }
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let arch = sample_archive();
        let bytes = arch.encode();
        let decoded = ModuleArchive::decode(&bytes).expect("decodes");
        assert_eq!(decoded, arch);
    }

    #[test]
    fn get_returns_the_right_chunk() {
        let arch = sample_archive();
        let bytes = arch.encode();
        let decoded = ModuleArchive::decode(&bytes).expect("decodes");
        assert_eq!(decoded.get("main"), Some(&[0xAA, 0xBB, 0xCC, 0x00, 0xFF][..]));
        assert_eq!(decoded.get("std/foo"), Some(&[1, 2, 3][..]));
        assert_eq!(decoded.get("pkg/bar/baz"), Some(&[][..]));
        assert_eq!(decoded.get("missing"), None);
    }

    #[test]
    fn empty_chunk_and_nonzero_entry_round_trip() {
        let arch = ModuleArchive {
            entry: 1,
            caps: CapSet::default(),
            shake_digest: [0u8; 32],
            modules: vec![
                ("a".to_string(), vec![9, 9]),
                ("b".to_string(), vec![7]),
            ],
        };
        let decoded = ModuleArchive::decode(&arch.encode()).expect("decodes");
        assert_eq!(decoded, arch);
        assert_eq!(decoded.entry, 1);
    }

    #[test]
    fn archive_magic_is_distinct() {
        assert_ne!(ARCHIVE_MAGIC, BUNDLE_MAGIC);
        assert_ne!(&ARCHIVE_MAGIC[..4], &ASO_MAGIC[..]);
        assert_eq!(&ARCHIVE_MAGIC, b"ASCRIPTA");
    }

    #[test]
    fn wrong_magic_is_clean_err() {
        // A bare-ish blob that isn't an archive — e.g. an .aso magic up front.
        let mut blob = vec![0u8; 64];
        blob[..4].copy_from_slice(&ASO_MAGIC);
        match ModuleArchive::decode(&blob) {
            Err(ArchiveError::BadMagic(_)) => {}
            other => panic!("expected BadMagic, got {other:?}"),
        }
        // A bundle magic must also not be mistaken for an archive.
        let mut bundle = vec![0u8; 64];
        bundle[..8].copy_from_slice(&BUNDLE_MAGIC);
        assert!(matches!(
            ModuleArchive::decode(&bundle),
            Err(ArchiveError::BadMagic(_))
        ));
    }

    #[test]
    fn version_mismatch_is_clean_err() {
        let mut bytes = sample_archive().encode();
        // Bump the version word (bytes 8..10) past what we support.
        bytes[8] = bytes[8].wrapping_add(1);
        match ModuleArchive::decode(&bytes) {
            Err(ArchiveError::VersionMismatch { expected, .. }) => {
                assert_eq!(expected, ARCHIVE_VERSION);
            }
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn every_truncation_prefix_is_clean_err_no_panic() {
        let full = sample_archive().encode();
        // Each strict prefix must decode to an Err, never panic.
        for n in 0..full.len() {
            let res = ModuleArchive::decode(&full[..n]);
            assert!(res.is_err(), "prefix len {n} should be an error");
        }
    }

    #[test]
    fn over_large_module_count_is_clean_err_no_panic() {
        // Hand-craft a minimal header with a huge module_count and no module data.
        let mut b = Vec::new();
        b.extend_from_slice(&ARCHIVE_MAGIC);
        b.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // entry
        let caps = CapSet::default().to_bytes();
        write_len(&mut b, caps.len());
        b.extend_from_slice(&caps);
        b.extend_from_slice(&[0u8; 32]); // shake digest
        // entry=0 with a huge count → TooManyModules fires first (the count cap precedes the
        // decode loop), NOT a truncation and NOT EntryOutOfRange.
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // module_count

        let res = ModuleArchive::decode(&b);
        assert!(res.is_err(), "over-large count must error, not OOM");
        // u32::MAX > MAX_MODULES, so it's the count cap specifically.
        assert!(matches!(res, Err(ArchiveError::TooManyModules(_))));
    }

    #[test]
    fn over_large_key_length_is_clean_err_no_panic() {
        let mut b = header_with_one_module_prefix();
        // A key_len far beyond the buffer.
        write_len(&mut b, u32::MAX as usize);
        // (no key bytes follow)
        let res = ModuleArchive::decode(&b);
        assert!(matches!(res, Err(ArchiveError::Truncated)));
    }

    #[test]
    fn over_large_chunk_length_is_clean_err_no_panic() {
        let mut b = header_with_one_module_prefix();
        write_len(&mut b, 1); // key_len = 1
        b.push(b'm'); // key = "m"
        write_len(&mut b, u32::MAX as usize); // chunk_len far beyond the buffer
        let res = ModuleArchive::decode(&b);
        assert!(matches!(res, Err(ArchiveError::Truncated)));
    }

    #[test]
    fn entry_out_of_range_is_clean_err() {
        let mut b = Vec::new();
        b.extend_from_slice(&ARCHIVE_MAGIC);
        b.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        b.extend_from_slice(&5u32.to_le_bytes()); // entry = 5
        let caps = CapSet::default().to_bytes();
        write_len(&mut b, caps.len());
        b.extend_from_slice(&caps);
        b.extend_from_slice(&[0u8; 32]);
        write_len(&mut b, 1); // module_count = 1 (entry 5 is out of range)
        match ModuleArchive::decode(&b) {
            Err(ArchiveError::EntryOutOfRange { entry, count }) => {
                assert_eq!(entry, 5);
                assert_eq!(count, 1);
            }
            other => panic!("expected EntryOutOfRange, got {other:?}"),
        }
    }

    #[test]
    fn entry_out_of_range_on_empty_modules() {
        // module_count = 0 means NO module can be the entry → entry 0 is out of range.
        let mut b = Vec::new();
        b.extend_from_slice(&ARCHIVE_MAGIC);
        b.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // entry = 0
        let caps = CapSet::default().to_bytes();
        write_len(&mut b, caps.len());
        b.extend_from_slice(&caps);
        b.extend_from_slice(&[0u8; 32]);
        write_len(&mut b, 0); // module_count = 0
        assert!(matches!(
            ModuleArchive::decode(&b),
            Err(ArchiveError::EntryOutOfRange { entry: 0, count: 0 })
        ));
    }

    #[test]
    fn invalid_utf8_key_is_clean_err() {
        let mut b = header_with_one_module_prefix();
        let bad = [0xFFu8, 0xFE, 0xFD];
        write_len(&mut b, bad.len()); // key_len
        b.extend_from_slice(&bad); // non-UTF-8 key
        write_len(&mut b, 0); // chunk_len = 0
        assert!(matches!(
            ModuleArchive::decode(&b),
            Err(ArchiveError::InvalidUtf8)
        ));
    }

    #[test]
    fn bad_caps_blob_is_clean_err() {
        // A caps sub-blob with a length that fits the buffer but is itself an invalid CapSet.
        let mut b = Vec::new();
        b.extend_from_slice(&ARCHIVE_MAGIC);
        b.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        // A zero-length caps blob: CapSet::from_bytes on empty input is Truncated.
        write_len(&mut b, 0);
        // (CapSet::from_bytes needs at least the bits byte → Truncated)
        let res = ModuleArchive::decode(&b);
        assert!(matches!(res, Err(ArchiveError::Caps(_))));
    }

    /// A well-formed header (magic · version · entry=0 · caps · shake · module_count=1) with the
    /// module body left to the caller to append.
    fn header_with_one_module_prefix() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&ARCHIVE_MAGIC);
        b.extend_from_slice(&ARCHIVE_VERSION.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // entry = 0
        let caps = CapSet::default().to_bytes();
        write_len(&mut b, caps.len());
        b.extend_from_slice(&caps);
        b.extend_from_slice(&[0u8; 32]); // shake digest
        write_len(&mut b, 1); // module_count = 1
        b
    }
}
