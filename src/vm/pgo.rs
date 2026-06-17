//! WARM Unit B — PGO section codec (spec §3.2).
//!
//! A [`PgoSection`] is a self-described *trailing section* appended after the module table of a
//! [`ModuleArchive`](crate::vm::archive::ModuleArchive). It is NOT part of the archive
//! `encode`/`decode` contract — it rides OUTSIDE those routines, preserving the Task 0 pin that
//! old readers ignore trailing bytes and therefore decode a PGO-carrying archive correctly (no
//! `ARCHIVE_VERSION` bump needed).
//!
//! ## Wire format (spec §3.2)
//!
//! ```text
//! section_frame:
//!   magic:        b"ASPGO\0\0\0"      (8 bytes)
//!   section_version: u16 LE           (PGO section's own minor tag; mismatch ⇒ skip)
//!   section_len:  u32 LE              (payload byte count — allows forward-skipping)
//!   payload:
//!     key_list_table: count:u32 · [ nkeys:u32 · [klen:u32 · key:utf8]× ]×
//!     module_count:   u32
//!     [ module_key:  klen:u32 · utf8
//!       chunk_sha256: [u8; 32]
//!       proto_count:  u32
//!       [ proto_path: depth:u8 · [u32]×depth
//!         arith:   n:u32 · [ off:u32 · kind:u8 ]×
//!         fields:  n:u32 · [ off:u32 · nlists:u8 · [list_idx:u32]× ]×
//!         globals: n:u32 · [ off:u32 ]× ]× ]×
//! ```
//!
//! ## Hostile-input safety
//!
//! [`PgoSection::decode`] returns `None` on ANY anomaly — unknown `section_version`, truncation,
//! out-of-range key-list index, UTF-8 error, or a count bomb. Count fields are clamped before any
//! allocation (the `archive.rs` discipline, mirrored exactly): `Vec::with_capacity` is always
//! bounded by `remaining_bytes`, so a malicious huge count cannot drive a multi-GB allocation.
//!
//! [`scan_trailing_sections`] in `archive.rs` provides the complementary frame scanner: it
//! iterates over self-described trailing sections after the module table, skipping unknown magics
//! by their declared `section_len` and stopping at the first malformed frame.

/// The 8-byte magic that identifies a PGO trailing section.
/// Distinct from `ASCRIPTA` / `ASO\0` / `ASCRIPTB`.
pub const PGO_SECTION_MAGIC: [u8; 8] = *b"ASPGO\0\0\0";

/// The PGO section's own format version tag. An unknown version causes the decoder
/// to return `None` (ignore and warm normally — never a load failure).
pub const PGO_SECTION_VERSION: u16 = 1;

/// Cap on counts read from untrusted PGO bytes, applied before `Vec::with_capacity`.
/// Generous enough for any real program; a hostile stream claiming > this many
/// key-lists / modules / protos triggers a clean `None`, not a huge allocation.
const MAX_PGO_ITEMS: usize = 1 << 20; // 1 M items

/// A decoded PGO profile section.
///
/// The section carries a deduped table of *key lists* (insertion-ordered string lists,
/// each a shape's field key layout), referenced by index from the field-IC records.
/// Modules are listed by their logical archive key and the sha256 of the stored chunk
/// bytes (so the seeder can reject a stale profile for a module whose bytecode changed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoSection {
    /// Deduped table of shape key lists. Each entry is an ordered `Vec<String>`.
    /// Field-IC records in [`PgoProto::fields`] reference entries here by index.
    pub key_lists: Vec<Vec<String>>,
    /// Per-module warm-state records.
    pub modules: Vec<PgoModule>,
}

/// Warm-state records for one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoModule {
    /// The module's logical archive key (e.g. `"main.as"`, `"util/math.as"`).
    pub module_key: String,
    /// sha256 of the module's stored `.aso` chunk bytes as they appeared in the
    /// archive at record time. The seeder rejects this module's seeds if the live
    /// chunk bytes hash to a different value.
    pub chunk_sha256: [u8; 32],
    /// Per-proto warm-state records (one per reachable `FnProto` in the chunk).
    pub protos: Vec<PgoProto>,
}

/// Warm-state records for one `FnProto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoProto {
    /// Index path from the chunk root to this proto through `chunk.protos`
    /// (empty = the root proto of the chunk).
    pub path: Vec<u32>,
    /// Adaptive-arithmetic cache seeds: `(bytecode_offset, ArithKind tag byte)`.
    /// Tag bytes: 0=Int, 1=Number, 2=Decimal, 3=ConcatStr (matches `ArithKind` discriminants).
    pub arith: Vec<(u32, u8)>,
    /// Field-IC cache seeds: `(bytecode_offset, key_list_indices)`.
    /// The *indices* reference entries in [`PgoSection::key_lists`].
    /// **NO field index is stored** — the seeder derives the index from the chunk's own
    /// const-pool operand at seed time (spec §3.3, the soundness keystone).
    pub fields: Vec<(u32, Vec<u32>)>,
    /// Builtin-resolved `GET_GLOBAL` offsets that were cached at record time.
    /// Only the *offset* is stored; the seeder re-resolves the builtin by reading the
    /// site's own name operand and installing `GlobalCache::Cached` from the live table.
    pub globals: Vec<u32>,
}

impl PgoSection {
    /// Encode this section as a complete self-framed section blob:
    /// `magic(8) · version(u16 LE) · section_len(u32 LE) · payload`.
    ///
    /// This returns the WHOLE frame (including magic/version/len header), ready to be
    /// appended after a `ModuleArchive::encode()` blob via [`append_section`].
    pub fn encode(&self) -> Vec<u8> {
        let payload = self.encode_payload();
        let payload_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);

        let mut out = Vec::with_capacity(8 + 2 + 4 + payload.len());
        out.extend_from_slice(&PGO_SECTION_MAGIC);
        out.extend_from_slice(&PGO_SECTION_VERSION.to_le_bytes());
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    /// Decode a PGO section from its *payload* bytes (the bytes AFTER the
    /// `magic·version·section_len` frame header — the caller has already validated
    /// the magic and version and extracted exactly `section_len` bytes).
    ///
    /// Returns `None` on ANY anomaly (truncation, bad UTF-8, out-of-range key-list
    /// index, count bomb with huge declared count vs tiny buffer).  Never panics.
    pub fn decode(payload: &[u8]) -> Option<PgoSection> {
        let mut r = PgoReader::new(payload);

        // --- key_list_table ---
        let kl_count = r.u32_clamped()? as usize;
        let mut key_lists = Vec::with_capacity(kl_count.min(r.remaining()));
        for _ in 0..kl_count {
            let nkeys = r.u32_clamped()? as usize;
            let mut keys = Vec::with_capacity(nkeys.min(r.remaining()));
            for _ in 0..nkeys {
                keys.push(r.str()?);
            }
            key_lists.push(keys);
        }

        // --- modules ---
        let mod_count = r.u32_clamped()? as usize;
        let mut modules = Vec::with_capacity(mod_count.min(r.remaining()));
        for _ in 0..mod_count {
            let module_key = r.str()?;
            let chunk_sha256: [u8; 32] = r.array32()?;

            let proto_count = r.u32_clamped()? as usize;
            let mut protos = Vec::with_capacity(proto_count.min(r.remaining()));
            for _ in 0..proto_count {
                // proto_path: depth:u8 · [u32]×depth
                let depth = r.u8()? as usize;
                if depth > MAX_PGO_ITEMS {
                    return None;
                }
                let mut path = Vec::with_capacity(depth);
                for _ in 0..depth {
                    path.push(r.u32_raw()?);
                }

                // arith: n:u32 · [ off:u32 · kind:u8 ]×
                let n_arith = r.u32_clamped()? as usize;
                let mut arith = Vec::with_capacity(n_arith.min(r.remaining() / 5));
                for _ in 0..n_arith {
                    let off = r.u32_raw()?;
                    let kind = r.u8()?;
                    // Validate kind byte (0..=3 are the four ArithKind variants)
                    if kind > 3 {
                        return None;
                    }
                    arith.push((off, kind));
                }

                // fields: n:u32 · [ off:u32 · nlists:u8 · [list_idx:u32]× ]×
                let n_fields = r.u32_clamped()? as usize;
                let mut fields = Vec::with_capacity(n_fields.min(r.remaining() / 5));
                for _ in 0..n_fields {
                    let off = r.u32_raw()?;
                    let nlists = r.u8()? as usize;
                    if nlists > MAX_PGO_ITEMS {
                        return None;
                    }
                    let mut list_indices = Vec::with_capacity(nlists.min(r.remaining() / 4));
                    for _ in 0..nlists {
                        let idx = r.u32_raw()?;
                        // Validate that the key-list index is in range
                        if idx as usize >= key_lists.len() {
                            return None;
                        }
                        list_indices.push(idx);
                    }
                    fields.push((off, list_indices));
                }

                // globals: n:u32 · [ off:u32 ]×
                let n_globals = r.u32_clamped()? as usize;
                let mut globals = Vec::with_capacity(n_globals.min(r.remaining() / 4));
                for _ in 0..n_globals {
                    globals.push(r.u32_raw()?);
                }

                protos.push(PgoProto {
                    path,
                    arith,
                    fields,
                    globals,
                });
            }

            modules.push(PgoModule {
                module_key,
                chunk_sha256,
                protos,
            });
        }

        Some(PgoSection { key_lists, modules })
    }

    // --- private helpers ---

    fn encode_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();

        // key_list_table: count:u32 · [ nkeys:u32 · [klen:u32 · key]× ]×
        write_u32(&mut out, self.key_lists.len());
        for klist in &self.key_lists {
            write_u32(&mut out, klist.len());
            for key in klist {
                write_str(&mut out, key);
            }
        }

        // modules
        write_u32(&mut out, self.modules.len());
        for m in &self.modules {
            write_str(&mut out, &m.module_key);
            out.extend_from_slice(&m.chunk_sha256);

            write_u32(&mut out, m.protos.len());
            for p in &m.protos {
                // proto_path: depth:u8 · [u32]×depth
                let depth = p.path.len().min(255) as u8;
                out.push(depth);
                for &idx in &p.path[..depth as usize] {
                    out.extend_from_slice(&idx.to_le_bytes());
                }

                // arith: n:u32 · [ off:u32 · kind:u8 ]×
                write_u32(&mut out, p.arith.len());
                for &(off, kind) in &p.arith {
                    out.extend_from_slice(&off.to_le_bytes());
                    out.push(kind);
                }

                // fields: n:u32 · [ off:u32 · nlists:u8 · [list_idx:u32]× ]×
                write_u32(&mut out, p.fields.len());
                for (off, idxs) in &p.fields {
                    out.extend_from_slice(&off.to_le_bytes());
                    let nlists = idxs.len().min(255) as u8;
                    out.push(nlists);
                    for &idx in &idxs[..nlists as usize] {
                        out.extend_from_slice(&idx.to_le_bytes());
                    }
                }

                // globals: n:u32 · [ off:u32 ]×
                write_u32(&mut out, p.globals.len());
                for &off in &p.globals {
                    out.extend_from_slice(&off.to_le_bytes());
                }
            }
        }

        out
    }
}

// ── Byte helpers ──────────────────────────────────────────────────────────────

fn write_u32(out: &mut Vec<u8>, n: usize) {
    let v = u32::try_from(n).unwrap_or(u32::MAX);
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    write_u32(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

// ── Hostile-safe reader ────────────────────────────────────────────────────────

/// A bounds-checked, forward-reading little-endian reader over untrusted PGO payload bytes.
/// Every accessor returns `None` on any truncation — no panics.
struct PgoReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PgoReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        PgoReader { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        let b = self.take(1)?;
        Some(b[0])
    }

    /// Read a raw u32 (no clamping — used for offsets, path indices, key-list indices
    /// where a large value is semantically valid or validated by a subsequent range check).
    fn u32_raw(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a count u32 and clamp it to [`MAX_PGO_ITEMS`].
    /// Returning `None` is reserved for truncation; a huge count that exceeds `MAX_PGO_ITEMS`
    /// is clamped and the loop will surface as a truncation when it tries to read the
    /// promised elements from a short buffer.
    ///
    /// This mirrors the `archive.rs` discipline exactly: declare the cap, let the
    /// loop hit `Truncated` naturally — never `with_capacity(huge)`.
    fn u32_clamped(&mut self) -> Option<u32> {
        let v = self.u32_raw()?;
        if v as usize > MAX_PGO_ITEMS {
            // Hostile count-bomb: reject immediately rather than reserving capacity for it.
            return None;
        }
        Some(v)
    }

    fn array32(&mut self) -> Option<[u8; 32]> {
        let slice = self.take(32)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(slice);
        Some(arr)
    }

    /// Read a u32-length-prefixed UTF-8 string. Returns `None` on truncation or bad UTF-8.
    fn str(&mut self) -> Option<String> {
        let len = self.u32_raw()? as usize;
        // Clamp to remaining to avoid huge with_capacity
        if len > self.remaining() {
            return None;
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }
}

// ── Archive-level section utilities ───────────────────────────────────────────

/// Append a pre-encoded section frame (as produced by [`PgoSection::encode`]) to an
/// already-encoded `ModuleArchive` byte blob.
///
/// This is a pure byte-append helper; it does NOT modify the archive's magic, version,
/// or module table — the section rides OUTSIDE `ModuleArchive::encode`/`decode`.
pub fn append_section(archive_bytes: &mut Vec<u8>, section_frame: &[u8]) {
    archive_bytes.extend_from_slice(section_frame);
}

/// Parsed entry from [`scan_trailing_sections`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailingSection {
    /// The 8-byte magic tag identifying the section type.
    pub magic: [u8; 8],
    /// The section's own version field.
    pub version: u16,
    /// The raw payload bytes (the bytes AFTER the `magic·version·len` header,
    /// exactly `section_len` bytes long).
    pub payload: Vec<u8>,
}

/// Scan the trailing bytes of an archive blob for self-described sections.
///
/// Starting at `start_offset` (the byte immediately after the last byte the
/// `ModuleArchive::decode` reader consumed), iterates over zero-or-more self-described
/// frames of the form `magic(8) · version(u16 LE) · len(u32 LE) · payload(len bytes)`.
///
/// - An unknown magic is skipped by its declared `len` (forward-compatible).
/// - A malformed frame (truncation, `len` that overflows the buffer) ENDS the scan
///   cleanly — no error, no panic.
///
/// Returns ALL recognised (or all, if `filter_magic` is `None`) trailing sections in
/// order.  Callers that only want the PGO section can pass `Some(PGO_SECTION_MAGIC)`.
pub fn scan_trailing_sections(bytes: &[u8], start_offset: usize) -> Vec<TrailingSection> {
    let mut sections = Vec::new();
    let mut pos = start_offset;

    loop {
        // Need at least 8 (magic) + 2 (version) + 4 (len) = 14 bytes for a valid frame.
        let remaining = bytes.len().saturating_sub(pos);
        if remaining < 14 {
            break;
        }

        // magic (8 bytes)
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&bytes[pos..pos + 8]);
        pos += 8;

        // version (u16 LE)
        let version = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;

        // section_len (u32 LE)
        let section_len = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        pos += 4;

        // Bounds-check: we need exactly `section_len` more bytes.
        let end = match pos.checked_add(section_len) {
            Some(e) if e <= bytes.len() => e,
            _ => break, // truncated frame — stop scan cleanly
        };

        let payload = bytes[pos..end].to_vec();
        pos = end;

        sections.push(TrailingSection {
            magic,
            version,
            payload,
        });
    }

    sections
}

/// Convenience: find the first PGO section in the trailing bytes and decode it.
/// Returns `None` if no PGO section is present, the version is unknown, or the
/// payload is malformed.
pub fn find_and_decode_pgo(bytes: &[u8], start_offset: usize) -> Option<PgoSection> {
    let sections = scan_trailing_sections(bytes, start_offset);
    for sec in sections {
        if sec.magic == PGO_SECTION_MAGIC {
            if sec.version != PGO_SECTION_VERSION {
                return None; // unknown version ⇒ ignore-and-warm-normally
            }
            return PgoSection::decode(&sec.payload);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Build a representative `PgoSection` that exercises all record types:
    /// - two key lists (one shared, dedup exercised)
    /// - two modules, one with nested proto paths
    fn sample_section() -> PgoSection {
        PgoSection {
            key_lists: vec![
                vec!["x".to_string(), "y".to_string(), "z".to_string()],
                vec!["name".to_string(), "age".to_string()],
            ],
            modules: vec![
                PgoModule {
                    module_key: "main.as".to_string(),
                    chunk_sha256: sha256_fixture(1),
                    protos: vec![
                        // Root proto: arith + fields + globals
                        PgoProto {
                            path: vec![],
                            arith: vec![(100, 0 /*Int*/), (200, 1 /*Number*/)],
                            fields: vec![
                                (50, vec![0]), // key-list index 0 = ["x","y","z"]
                                (60, vec![0, 1]), // Poly: both key lists
                            ],
                            globals: vec![300, 400],
                        },
                        // Nested proto (path [0, 2])
                        PgoProto {
                            path: vec![0, 2],
                            arith: vec![(10, 2 /*Decimal*/)],
                            fields: vec![],
                            globals: vec![],
                        },
                    ],
                },
                PgoModule {
                    module_key: "util/math.as".to_string(),
                    chunk_sha256: sha256_fixture(2),
                    protos: vec![PgoProto {
                        path: vec![],
                        arith: vec![(5, 3 /*ConcatStr*/)],
                        fields: vec![(8, vec![1])], // key-list index 1 = ["name","age"]
                        globals: vec![99],
                    }],
                },
            ],
        }
    }

    /// A deterministic [u8;32] fixture keyed by `seed`.
    fn sha256_fixture(seed: u8) -> [u8; 32] {
        let mut arr = [0u8; 32];
        for (i, b) in arr.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(seed).wrapping_add(seed);
        }
        arr
    }

    // ── Round-trip ────────────────────────────────────────────────────────────

    #[test]
    fn encode_decode_round_trip() {
        let orig = sample_section();
        let frame = orig.encode();
        // The frame starts with the magic + version + len header
        assert_eq!(&frame[..8], &PGO_SECTION_MAGIC);
        assert_eq!(
            u16::from_le_bytes([frame[8], frame[9]]),
            PGO_SECTION_VERSION
        );
        let payload_len = u32::from_le_bytes([frame[10], frame[11], frame[12], frame[13]]) as usize;
        assert_eq!(payload_len, frame.len() - 14);

        // Decode the payload (everything after the 14-byte header)
        let decoded = PgoSection::decode(&frame[14..]).expect("round-trip must succeed");
        assert_eq!(decoded, orig);
    }

    #[test]
    fn empty_section_round_trip() {
        let empty = PgoSection {
            key_lists: vec![],
            modules: vec![],
        };
        let frame = empty.encode();
        let decoded = PgoSection::decode(&frame[14..]).expect("empty round-trip");
        assert_eq!(decoded, empty);
    }

    #[test]
    fn key_list_dedup_preserved() {
        // Two field records share the SAME key-list index — the table stores it once.
        let sec = PgoSection {
            key_lists: vec![vec!["a".to_string(), "b".to_string()]],
            modules: vec![PgoModule {
                module_key: "m.as".to_string(),
                chunk_sha256: [0u8; 32],
                protos: vec![PgoProto {
                    path: vec![],
                    arith: vec![],
                    fields: vec![
                        (10, vec![0]), // both reference key-list 0
                        (20, vec![0]),
                    ],
                    globals: vec![],
                }],
            }],
        };
        let frame = sec.encode();
        let decoded = PgoSection::decode(&frame[14..]).expect("dedup round-trip");
        assert_eq!(decoded, sec);
        assert_eq!(decoded.key_lists.len(), 1); // still exactly 1 key list
    }

    // ── Hostile-safe decode ───────────────────────────────────────────────────

    #[test]
    fn truncation_at_every_offset_returns_none_never_panics() {
        let frame = sample_section().encode();
        let payload = &frame[14..];
        // Every strict prefix of the payload must decode to `None`, never panic.
        // (The empty slice also returns None — no modules were read.)
        for n in 0..payload.len() {
            let result = PgoSection::decode(&payload[..n]);
            assert!(
                result.is_none(),
                "payload prefix of len {n} unexpectedly decoded to Some"
            );
        }
    }

    #[test]
    fn unknown_version_returns_none() {
        let mut frame = sample_section().encode();
        // Overwrite the version field (bytes 8..10) with an unknown version
        frame[8] = 0xFF;
        frame[9] = 0xFF;
        // find_and_decode_pgo should return None for unknown version
        let mut archive_bytes = crate::vm::archive::ModuleArchive::new(
            0,
            crate::stdlib::caps::CapSet::all_granted(),
            [0u8; 32],
            vec![("main.as".to_string(), vec![1, 2, 3])],
        )
        .encode();
        let start = archive_bytes.len();
        archive_bytes.extend_from_slice(&frame);
        let result = find_and_decode_pgo(&archive_bytes, start);
        assert!(result.is_none(), "unknown version must return None");
    }

    #[test]
    fn out_of_range_key_list_index_returns_none() {
        // A field record that references a key-list index beyond the table must fail.
        let sec = PgoSection {
            key_lists: vec![vec!["x".to_string()]], // only index 0 is valid
            modules: vec![PgoModule {
                module_key: "m.as".to_string(),
                chunk_sha256: [0u8; 32],
                protos: vec![PgoProto {
                    path: vec![],
                    arith: vec![],
                    fields: vec![(10, vec![0, 1])], // index 1 is OUT OF RANGE
                    globals: vec![],
                }],
            }],
        };
        let frame = sec.encode();
        // The encoder faithfully writes index 1; the decoder must reject it.
        let result = PgoSection::decode(&frame[14..]);
        assert!(result.is_none(), "out-of-range key-list index must return None");
    }

    #[test]
    fn count_bomb_huge_key_list_count_returns_none() {
        // Hand-craft a payload with a key_list_table count of u32::MAX.
        let mut payload = Vec::new();
        payload.extend_from_slice(&u32::MAX.to_le_bytes()); // count bomb
        // (no actual key-list data follows)
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "count bomb must return None, not OOM");
    }

    #[test]
    fn count_bomb_huge_module_count_returns_none() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // 0 key lists (valid)
        payload.extend_from_slice(&u32::MAX.to_le_bytes()); // huge module count
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "huge module count must return None");
    }

    #[test]
    fn count_bomb_huge_arith_count_returns_none() {
        let mut payload = Vec::new();
        // 0 key lists
        payload.extend_from_slice(&0u32.to_le_bytes());
        // 1 module
        payload.extend_from_slice(&1u32.to_le_bytes());
        // module_key "m.as"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"m.as");
        // chunk_sha256
        payload.extend_from_slice(&[0u8; 32]);
        // 1 proto
        payload.extend_from_slice(&1u32.to_le_bytes());
        // proto_path: depth=0
        payload.push(0u8);
        // arith: huge count
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "huge arith count must return None");
    }

    #[test]
    fn count_bomb_huge_fields_count_returns_none() {
        let mut payload = Vec::new();
        // 0 key lists
        payload.extend_from_slice(&0u32.to_le_bytes());
        // 1 module
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"m.as");
        payload.extend_from_slice(&[0u8; 32]);
        // 1 proto
        payload.extend_from_slice(&1u32.to_le_bytes());
        // proto_path: depth=0
        payload.push(0u8);
        // arith: 0
        payload.extend_from_slice(&0u32.to_le_bytes());
        // fields: huge count
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "huge fields count must return None");
    }

    #[test]
    fn invalid_arith_kind_byte_returns_none() {
        let mut payload = Vec::new();
        // 0 key lists, 1 module
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"m.as");
        payload.extend_from_slice(&[0u8; 32]);
        // 1 proto
        payload.extend_from_slice(&1u32.to_le_bytes());
        // depth = 0
        payload.push(0u8);
        // 1 arith entry with invalid kind byte = 99
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&42u32.to_le_bytes()); // offset
        payload.push(99u8); // invalid kind
        // fields: 0, globals: 0 (we never reach these)
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "invalid arith kind byte must return None");
    }

    #[test]
    fn bad_utf8_key_returns_none() {
        let mut payload = Vec::new();
        // 0 key lists, 1 module with a bad UTF-8 key
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes()); // key length 3
        payload.extend_from_slice(&[0xFF, 0xFE, 0xFD]); // non-UTF-8
        let result = PgoSection::decode(&payload);
        assert!(result.is_none(), "bad UTF-8 key must return None");
    }

    // ── scan_trailing_sections ────────────────────────────────────────────────

    #[test]
    fn scan_trailing_sections_returns_all_valid_sections() {
        let archive = crate::vm::archive::ModuleArchive::new(
            0,
            crate::stdlib::caps::CapSet::all_granted(),
            [0u8; 32],
            vec![("main.as".to_string(), vec![1, 2, 3])],
        );
        let mut bytes = archive.encode();
        let start = bytes.len();

        // Append two sections: a PGO section and a made-up future section.
        let pgo_frame = sample_section().encode();
        bytes.extend_from_slice(&pgo_frame);

        // A fake "FUTURE" section
        bytes.extend_from_slice(b"FUTURESE"); // 8-byte magic
        bytes.extend_from_slice(&42u16.to_le_bytes()); // version
        let fake_payload = b"hello world";
        bytes.extend_from_slice(&(fake_payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(fake_payload);

        let sections = scan_trailing_sections(&bytes, start);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].magic, PGO_SECTION_MAGIC);
        assert_eq!(sections[0].version, PGO_SECTION_VERSION);
        assert_eq!(sections[1].magic, *b"FUTURESE");
        assert_eq!(sections[1].version, 42);
        assert_eq!(sections[1].payload, fake_payload);
    }

    #[test]
    fn scan_trailing_sections_unknown_magic_skipped_by_length() {
        let mut bytes = vec![0u8; 0]; // No archive prefix needed for this test
        // Unknown magic section (should be skipped)
        bytes.extend_from_slice(b"UNKNOWN1");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        let payload = b"skip me";
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        // PGO section after it
        let pgo_frame = sample_section().encode();
        bytes.extend_from_slice(&pgo_frame);

        let sections = scan_trailing_sections(&bytes, 0);
        // Both sections are returned (scan returns all, caller filters)
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].magic, *b"UNKNOWN1");
        assert_eq!(sections[1].magic, PGO_SECTION_MAGIC);
    }

    #[test]
    fn scan_stops_cleanly_on_malformed_frame() {
        let mut bytes = vec![0u8; 0];
        // Valid section
        bytes.extend_from_slice(b"SECTION1");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        let p = b"ok";
        bytes.extend_from_slice(&(p.len() as u32).to_le_bytes());
        bytes.extend_from_slice(p);
        // Malformed: truncated (only magic + partial version — 8+1 bytes = 9, need 14+)
        bytes.extend_from_slice(&[0xAA; 9]);

        let sections = scan_trailing_sections(&bytes, 0);
        // The scan found the first valid one, then stopped at the malformed frame.
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].magic, *b"SECTION1");
    }

    #[test]
    fn scan_stops_cleanly_on_frame_len_overflow() {
        let mut bytes = vec![0u8; 0];
        // A section whose `section_len` exceeds the remaining buffer
        bytes.extend_from_slice(b"OVERFLOW");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // huge len
        // (no payload follows)

        let sections = scan_trailing_sections(&bytes, 0);
        assert!(sections.is_empty(), "overflow frame should stop scan cleanly");
    }

    #[test]
    fn scan_empty_tail_returns_empty() {
        let sections = scan_trailing_sections(&[0u8; 0], 0);
        assert!(sections.is_empty());
    }

    #[test]
    fn find_and_decode_pgo_round_trip() {
        let orig = sample_section();
        let archive = crate::vm::archive::ModuleArchive::new(
            0,
            crate::stdlib::caps::CapSet::all_granted(),
            [0u8; 32],
            vec![("main.as".to_string(), vec![1])],
        );
        let mut bytes = archive.encode();
        let start = bytes.len();
        append_section(&mut bytes, &orig.encode());

        let decoded = find_and_decode_pgo(&bytes, start).expect("should find and decode");
        assert_eq!(decoded, orig);
    }

    #[test]
    fn append_section_is_pure_byte_append() {
        let archive = crate::vm::archive::ModuleArchive::new(
            0,
            crate::stdlib::caps::CapSet::all_granted(),
            [0u8; 32],
            vec![("main.as".to_string(), vec![7, 8, 9])],
        );
        let encoded = archive.encode();
        let mut with_section = encoded.clone();
        let frame = sample_section().encode();
        append_section(&mut with_section, &frame);

        // The archive bytes are untouched (the section is purely appended)
        assert_eq!(&with_section[..encoded.len()], encoded.as_slice());
        // And the archive still decodes correctly
        let decoded = crate::vm::archive::ModuleArchive::decode(&with_section)
            .expect("archive with trailing PGO section must still decode");
        assert_eq!(decoded, archive);
    }
}
