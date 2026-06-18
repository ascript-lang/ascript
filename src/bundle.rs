//! BIN — native single-binary bundle codec (spec §2.1/§2.5).
//!
//! `ascript build --native app.as -o app` produces `stub || payload || footer`, where
//! `stub` is a copy of the running runtime (`current_exe()`), `payload` is the *verified*
//! `.aso` bytes, and `footer` is a fixed-size, magic-tagged trailing struct. On startup the
//! runtime reads the last [`FOOTER_SIZE`] bytes of its own image; if they carry
//! [`BUNDLE_MAGIC`], it slices `payload` and runs it through the SAME `from_bytes_verified`
//! trust boundary as `run file.aso`.
//!
//! This module is the codec + the macOS ad-hoc signer. It is **pure** (no I/O except the
//! signer) and every read is **bounds-checked** — `read_bundle_footer` over attacker-editable
//! bytes must NEVER panic or slice out of bounds, only ever return `Some(bounds)` or `None`.

use std::path::Path;

/// The bundle footer magic — DISTINCT from `ASO_MAGIC` (`b"ASO\0"`) so a bare `.aso` file
/// (no stub, no footer) is never mistaken for a bundle, and vice-versa.
pub const BUNDLE_MAGIC: [u8; 8] = *b"ASCRIPTB";

/// The footer layout version (bump only if the footer struct changes — independent of
/// `ASO_FORMAT_VERSION`, which versions the embedded payload). RT §7.2: writers emit
/// version `1` when `flags == 0` (bit-identical to every pre-RT bundle) and version `2`
/// when any flag bit is set. This constant is the v1 baseline; [`write_footer`] picks the
/// emitted version from `flags`.
pub const BUNDLE_FOOTER_VERSION: u16 = 1;

/// RT §7.1: the highest footer layout version this runtime can run. A footer claiming a
/// version above this is REFUSED loudly (`built by a newer ascript`).
pub const BUNDLE_FOOTER_VERSION_MAX: u16 = 2;

/// RT §7: footer `flags` bit — the payload is a compressed container
/// (`uncompressed_len:u64 LE || <one zstd frame>`), not a bare `.aso`/archive. Set by
/// `build --native --compress`; the shim decompresses before the magic dispatch.
pub const FLAG_ZSTD: u16 = 0x0001;

/// The full set of flag bits THIS runtime understands. A v2 footer with any bit OUTSIDE
/// this mask is refused (`this bundle uses features this runtime does not understand`).
const KNOWN_FLAGS: u16 = FLAG_ZSTD;

/// RT §7.3 sanity cap on a compressed payload's declared `uncompressed_len`. A bundle's
/// embedded `.aso`/archive is realistically tens of MB; a lie far above this is rejected
/// BEFORE any allocation (no attacker-driven OOM). Mirrors the `.aso` P0 clamp discipline.
#[cfg(feature = "bundle-zstd")]
const MAX_UNCOMPRESSED_PAYLOAD: u64 = 512 * 1024 * 1024;

/// RT §7: the PINNED zstd level for `compress_payload`. Fixed so a `--compress` build is
/// byte-deterministic (reproducible artifacts). Level 19 is a strong-ratio, still-practical
/// one-shot-build choice (the cost is paid once, at build time).
#[cfg(feature = "bundle-zstd")]
const BUNDLE_ZSTD_LEVEL: i32 = 19;

/// A conservative floor on the stub (runtime copy) size. The real `ascript` runtime is tens
/// of MB; a `payload_offset` below this is structurally impossible and is rejected (defends
/// against a footer that points the payload into the very start of the image).
pub const MIN_STUB_SIZE: u64 = 4096;

/// The fixed footer size in bytes: `payload_offset(8) + payload_len(8) + aso_version(4) +
/// bundle_version(2) + flags(2) + magic(8)`. The `flags` field occupies the SAME 2 bytes
/// at offset 22 that `reserved` did pre-RT (the wire layout is UNCHANGED — only the field's
/// meaning is now defined).
pub const FOOTER_SIZE: usize = 32;

/// The fixed-size trailing footer (all fields little-endian on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleFooter {
    /// Byte offset of the payload within the image (== the stub length).
    pub payload_offset: u64,
    /// Length of the embedded `.aso` payload in bytes.
    pub payload_len: u64,
    /// The `ASO_FORMAT_VERSION` of the embedded payload (informational; the real gate is
    /// `from_bytes_verified`, which re-checks the version itself).
    pub aso_version: u32,
    /// The footer layout version (1 when `flags == 0`, 2 otherwise — RT §7.2).
    pub bundle_version: u16,
    /// Feature flags (RT §7). `0` for a plain bundle (the pre-RT value); a nonzero value
    /// (e.g. [`FLAG_ZSTD`]) marks an extended bundle and forces `bundle_version = 2`. Same
    /// 2 wire bytes the pre-RT `reserved` field occupied.
    pub flags: u16,
    /// [`BUNDLE_MAGIC`].
    pub magic: [u8; 8],
}

impl BundleFooter {
    /// Serialize to the fixed `FOOTER_SIZE`-byte little-endian wire form.
    pub fn to_bytes(&self) -> [u8; FOOTER_SIZE] {
        let mut b = [0u8; FOOTER_SIZE];
        b[0..8].copy_from_slice(&self.payload_offset.to_le_bytes());
        b[8..16].copy_from_slice(&self.payload_len.to_le_bytes());
        b[16..20].copy_from_slice(&self.aso_version.to_le_bytes());
        b[20..22].copy_from_slice(&self.bundle_version.to_le_bytes());
        b[22..24].copy_from_slice(&self.flags.to_le_bytes());
        b[24..32].copy_from_slice(&self.magic);
        b
    }

    /// Parse from exactly `FOOTER_SIZE` bytes. Returns `None` on a wrong length or a magic
    /// mismatch (NO panic — `buf` is attacker-controlled).
    pub fn from_bytes(buf: &[u8]) -> Option<BundleFooter> {
        if buf.len() != FOOTER_SIZE {
            return None;
        }
        let magic: [u8; 8] = buf[24..32].try_into().ok()?;
        if magic != BUNDLE_MAGIC {
            return None;
        }
        Some(BundleFooter {
            payload_offset: u64::from_le_bytes(buf[0..8].try_into().ok()?),
            payload_len: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            aso_version: u32::from_le_bytes(buf[16..20].try_into().ok()?),
            bundle_version: u16::from_le_bytes(buf[20..22].try_into().ok()?),
            flags: u16::from_le_bytes(buf[22..24].try_into().ok()?),
            magic,
        })
    }
}

/// Build the footer for a bundle whose stub is `stub_len` bytes and whose payload is
/// `payload_len` bytes (the payload sits at offset `stub_len`, immediately after the stub).
/// `aso_version` is the embedded payload's `ASO_FORMAT_VERSION` (informational).
///
/// RT §7.2 versioning rule: `flags == 0` ⇒ `bundle_version = 1` (BIT-IDENTICAL to every
/// pre-RT bundle — a plain `--native` build is unchanged); any nonzero `flags` ⇒
/// `bundle_version = 2`. The version is DERIVED from `flags`, never passed independently,
/// so a v1 footer can never carry flags (a contradiction the reader treats as tampering).
pub fn write_footer(
    stub_len: u64,
    payload_len: u64,
    aso_version: u32,
    flags: u16,
) -> [u8; FOOTER_SIZE] {
    BundleFooter {
        payload_offset: stub_len,
        payload_len,
        aso_version,
        bundle_version: if flags == 0 { 1 } else { 2 },
        flags,
        magic: BUNDLE_MAGIC,
    }
    .to_bytes()
}

/// RT §7.2 — the three-way result of validating a footer tail. The codec is the single
/// trust boundary for the version/flags strictness matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterCheck {
    /// No `ASCRIPTB` footer (wrong magic / too short / out-of-bounds region). The binary is
    /// NOT a bundle — the caller runs as a normal toolchain launch (fall through to clap).
    /// This is the pre-RT `None` behavior and MUST NOT regress.
    NotABundle,
    /// A valid, runnable bundle: the bounds-checked payload region + its feature `flags`.
    Bundle { offset: u64, len: u64, flags: u16 },
    /// The footer IS an `ASCRIPTB` footer but this runtime REFUSES to run it (corruption,
    /// tampering, an unknown flag bit, a too-new version, or compressed-without-support).
    /// The carried string is a user-facing, reported error — never a silent fall-through.
    Refused(String),
}

/// Read the trailing footer of an executable image and return the **bounds-checked**
/// `(payload_offset, payload_len)` of the embedded `.aso`, or `None` if `exe_bytes` is not a
/// valid bundle. O(1): inspects only the last `FOOTER_SIZE` bytes. NEVER panics / slices OOB
/// over attacker-editable `exe_bytes`. A `None` means "run as a normal `ascript` launch".
///
/// Validity requires ALL of:
/// - `exe_bytes.len() >= FOOTER_SIZE` (room for a footer);
/// - the trailing magic is [`BUNDLE_MAGIC`];
/// - `payload_offset >= MIN_STUB_SIZE` (a real stub is large);
/// - `payload_offset + payload_len` does not overflow AND `<= exe_len - FOOTER_SIZE` (the
///   payload region lies strictly within the image, before the footer).
pub fn read_bundle_footer(exe_bytes: &[u8]) -> Option<(usize, usize)> {
    let exe_len = exe_bytes.len();
    if exe_len < FOOTER_SIZE {
        return None;
    }
    match validate_footer(&exe_bytes[exe_len - FOOTER_SIZE..], exe_len as u64) {
        FooterCheck::Bundle { offset, len, .. } => Some((offset as usize, len as usize)),
        // A Refused footer is still NOT a runnable region for this helper's callers
        // (`build_native`'s old-overlay strip): it has a confirmed magic so the bytes ARE
        // a bundle overlay, but its layout/version is one we can't trust to slice. Treat it
        // as "no recoverable clean stub here" → `None` (the caller keeps the whole image,
        // which is the conservative, pre-RT behavior for an unrecognized tail).
        FooterCheck::Refused(_) | FooterCheck::NotABundle => None,
    }
}

/// Validate a `FOOTER_SIZE`-byte footer tail against the TOTAL image length `exe_len`,
/// returning the bounds-checked `(payload_offset, payload_len)` or `None`. This is the core
/// of [`read_bundle_footer`]; the startup shim ([`crate::try_run_embedded`]) uses it directly
/// so it can validate from just the file length + a small tail read, WITHOUT loading the
/// whole (tens-of-MB) executable on every launch (Task 7 startup budget). NEVER panics.
pub fn validate_footer(footer_tail: &[u8], exe_len: u64) -> FooterCheck {
    if exe_len < FOOTER_SIZE as u64 {
        return FooterCheck::NotABundle;
    }
    let footer = match BundleFooter::from_bytes(footer_tail) {
        Some(f) => f,
        // Wrong length or wrong magic — not a bundle at all (a plain toolchain launch).
        None => return FooterCheck::NotABundle,
    };

    // --- Structural bounds (these decide bundle-vs-not BEFORE strictness). A magic match
    // with an impossible region is still NotABundle: pre-RT a garbage region was `None` →
    // clap fall-through, and we must not turn that into a loud refusal. ---
    if footer.payload_offset < MIN_STUB_SIZE {
        return FooterCheck::NotABundle;
    }
    let region = exe_len - FOOTER_SIZE as u64;
    let end = match footer.payload_offset.checked_add(footer.payload_len) {
        Some(e) => e,
        None => return FooterCheck::NotABundle, // overflow → not a valid region
    };
    if end > region {
        return FooterCheck::NotABundle;
    }

    // --- §7.2 strictness matrix (magic + bounds confirmed: this IS a bundle; from here a
    // version/flags problem is a LOUD refusal, never a silent fall-through). ---
    if footer.bundle_version > BUNDLE_FOOTER_VERSION_MAX {
        return FooterCheck::Refused(format!(
            "this bundle was built by a newer ascript (footer version {}, this runtime \
             understands up to {}) — upgrade the runtime to run it",
            footer.bundle_version, BUNDLE_FOOTER_VERSION_MAX
        ));
    }
    if footer.bundle_version <= 1 {
        // v1 (or v0): a pre-RT writer ALWAYS wrote flags=0. A v1 footer carrying flags is
        // corruption or tampering — refuse rather than guess.
        if footer.flags != 0 {
            return FooterCheck::Refused(format!(
                "corrupt or tampered bundle: a version-{} footer carries flags 0x{:04x}, but \
                 such bundles always have flags 0",
                footer.bundle_version, footer.flags
            ));
        }
    } else {
        // v2: only flag bits this runtime understands are allowed.
        let unknown = footer.flags & !KNOWN_FLAGS;
        if unknown != 0 {
            return FooterCheck::Refused(format!(
                "this bundle uses features this runtime does not understand (flags 0x{:04x}) \
                 — upgrade the runtime to run it",
                footer.flags
            ));
        }
        // A compressed payload requires the `bundle-zstd` codec to be compiled in.
        #[cfg(not(feature = "bundle-zstd"))]
        if footer.flags & FLAG_ZSTD != 0 {
            return FooterCheck::Refused(
                "this runtime was built without compressed-bundle support (rebuild with the \
                 `bundle-zstd` feature, or use an uncompressed bundle)"
                    .to_string(),
            );
        }
    }

    FooterCheck::Bundle {
        offset: footer.payload_offset,
        len: footer.payload_len,
        flags: footer.flags,
    }
}

/// Ad-hoc sign a freshly-written macOS Mach-O in place (the `codesign -s -` equivalent, via
/// the `apple-codesign` crate — no Xcode). REQUIRED on arm64: appending the payload
/// invalidated the stub's signature, so the kernel `SIGKILL`s an unsigned image at exec.
/// A no-op on non-macOS targets.
#[cfg(target_os = "macos")]
pub fn adhoc_sign_macos(path: &Path) -> Result<(), String> {
    use apple_codesign::{SigningSettings, UnifiedSigner};
    let settings = SigningSettings::default(); // identity unset => ad-hoc ("-")
    let signer = UnifiedSigner::new(settings);
    signer
        .sign_path_in_place(path)
        .map_err(|e| format!("ad-hoc code-signing failed for {}: {e}", path.display()))
}

/// Non-macOS: nothing to sign.
#[cfg(not(target_os = "macos"))]
pub fn adhoc_sign_macos(_path: &Path) -> Result<(), String> {
    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────────────────
// RT §7 — compressed-payload codec ([`FLAG_ZSTD`]).
//
// Wire form: `uncompressed_len:u64 LE || <one zstd frame>`. The length prefix lets the
// decoder pre-validate the declared size against a sanity cap BEFORE allocating, and verify
// the decoded size EXACTLY matches afterwards — a lie (too big OR too small) is a clean
// error, never an OOM or a truncated/over-read payload.
// ───────────────────────────────────────────────────────────────────────────────────────

/// RT §7 — compress a bundle payload (a verified `.aso` chunk or `ASCRIPTA` archive) into the
/// `uncompressed_len:u64 LE || <one zstd frame>` container. Single-threaded, PINNED level
/// ([`BUNDLE_ZSTD_LEVEL`]) for reproducible output.
#[cfg(feature = "bundle-zstd")]
pub fn compress_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let frame = zstd::encode_all(bytes, BUNDLE_ZSTD_LEVEL)
        .map_err(|e| format!("failed to compress bundle payload: {e}"))?;
    let mut out = Vec::with_capacity(8 + frame.len());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&frame);
    Ok(out)
}

/// RT §7 / §7.3 — decompress a [`compress_payload`] container back to the original payload.
///
/// HARDENED against hostile bytes (the container is attacker-controllable):
/// - reads the 8-byte `uncompressed_len` prefix (length-checked);
/// - rejects a declared length above [`MAX_UNCOMPRESSED_PAYLOAD`] BEFORE allocating (no
///   attacker-driven over-allocation / OOM);
/// - streams the zstd frame through a `Decoder` capped at `declared + 1` bytes (so an
///   under-declared length that decodes to MORE is caught without unbounded growth);
/// - errors unless the decoded length EXACTLY equals the declared length.
///
/// NO reachable `unwrap`/`panic!`.
#[cfg(feature = "bundle-zstd")]
pub fn decompress_payload(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;

    if data.len() < 8 {
        return Err("compressed bundle payload is truncated (missing length prefix)".to_string());
    }
    let declared = u64::from_le_bytes(
        data[0..8]
            .try_into()
            .map_err(|_| "compressed bundle payload has a malformed length prefix".to_string())?,
    );
    // §7.3 sanity cap — refuse a lie BEFORE allocating anything.
    if declared > MAX_UNCOMPRESSED_PAYLOAD {
        return Err(format!(
            "compressed bundle declares an implausible uncompressed size ({declared} bytes, \
             cap {MAX_UNCOMPRESSED_PAYLOAD}) — refusing (corrupt or hostile bundle)"
        ));
    }
    let declared = declared as usize;

    let frame = &data[8..];
    let mut decoder = zstd::stream::Decoder::new(frame)
        .map_err(|e| format!("compressed bundle payload is not a valid zstd frame: {e}"))?;

    // Cap the reader at declared+1: a correct frame decodes to exactly `declared`; an
    // under-declared frame that wants to produce more trips the cap and is rejected — the
    // buffer never grows past declared+1, so there is no over-allocation regardless of the
    // frame's internal claims.
    let mut out = Vec::with_capacity(declared);
    let read = decoder
        .by_ref()
        .take(declared as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("failed to decompress bundle payload: {e}"))?;

    if read != declared {
        return Err(format!(
            "compressed bundle length mismatch: declared {declared} bytes, decoded {read} \
             (corrupt or tampered bundle)"
        ));
    }
    Ok(out)
}

/// RT §7 — `compress_payload` when the runtime was built WITHOUT `bundle-zstd`. A
/// `--compress` build requires the codec; this is the loud, tested refusal.
#[cfg(not(feature = "bundle-zstd"))]
pub fn compress_payload(_bytes: &[u8]) -> Result<Vec<u8>, String> {
    Err("this runtime was built without compressed-bundle support (rebuild with the \
         `bundle-zstd` feature to use --compress)"
        .to_string())
}

/// RT §7 — `decompress_payload` when the runtime was built WITHOUT `bundle-zstd`. Reached
/// only if a `FLAG_ZSTD` bundle somehow passed `validate_footer` on a zstd-less stub (it
/// will not — `validate_footer` refuses `FLAG_ZSTD` first); kept as a belt-and-braces floor.
#[cfg(not(feature = "bundle-zstd"))]
pub fn decompress_payload(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("this runtime was built without compressed-bundle support".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current `.aso` format version, used for the informational `aso_version`
    /// footer field in these fixtures (bound to the live constant so it never goes stale
    /// on an `ASO_FORMAT_VERSION` bump).
    const AV: u32 = crate::vm::aso::ASO_FORMAT_VERSION;

    /// A synthetic bundle image: `stub_len` zero stub bytes + `payload` + the footer.
    fn make_image(stub_len: usize, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; stub_len];
        v.extend_from_slice(payload);
        v.extend_from_slice(&write_footer(stub_len as u64, payload.len() as u64, AV, 0));
        v
    }

    #[test]
    fn write_footer_round_trips_through_read() {
        let payload = b"the embedded .aso payload bytes";
        let stub_len = MIN_STUB_SIZE as usize + 100;
        let img = make_image(stub_len, payload);
        let (off, len) = read_bundle_footer(&img).expect("valid bundle");
        assert_eq!(off, stub_len);
        assert_eq!(len, payload.len());
        assert_eq!(&img[off..off + len], payload);
    }

    #[test]
    fn footer_struct_round_trips() {
        let f = BundleFooter {
            payload_offset: 12345,
            payload_len: 678,
            aso_version: AV,
            bundle_version: BUNDLE_FOOTER_VERSION,
            flags: 0,
            magic: BUNDLE_MAGIC,
        };
        assert_eq!(BundleFooter::from_bytes(&f.to_bytes()), Some(f));
    }

    #[test]
    fn non_bundle_blob_is_none() {
        // A large blob with no trailing ASCRIPTB magic.
        let blob = vec![0xABu8; (MIN_STUB_SIZE as usize) + 500];
        assert_eq!(read_bundle_footer(&blob), None);
    }

    #[test]
    fn file_shorter_than_footer_is_none() {
        for n in [0usize, 1, FOOTER_SIZE - 1] {
            assert_eq!(read_bundle_footer(&vec![0u8; n]), None, "len {n}");
        }
    }

    #[test]
    fn payload_region_past_eof_is_none_no_panic() {
        // A well-formed footer whose payload_offset + payload_len exceeds the image.
        let stub_len = MIN_STUB_SIZE;
        let mut img = vec![0u8; stub_len as usize + 16]; // small payload region
        // footer claims a payload far larger than what's present.
        img.extend_from_slice(&write_footer(stub_len, 1_000_000, AV, 0));
        assert_eq!(read_bundle_footer(&img), None);
    }

    #[test]
    fn overflowing_offset_plus_len_is_none() {
        // payload_offset + payload_len overflows u64 — must be rejected, not wrapped.
        let mut img = vec![0u8; MIN_STUB_SIZE as usize + 8];
        let footer = BundleFooter {
            payload_offset: u64::MAX - 10,
            payload_len: u64::MAX - 10,
            aso_version: AV,
            bundle_version: BUNDLE_FOOTER_VERSION,
            flags: 0,
            magic: BUNDLE_MAGIC,
        };
        img.extend_from_slice(&footer.to_bytes());
        assert_eq!(read_bundle_footer(&img), None);
    }

    #[test]
    fn payload_offset_below_min_stub_is_none() {
        // A footer whose payload starts before MIN_STUB_SIZE — structurally impossible.
        let payload = b"x";
        let mut img = vec![0u8; 100];
        img.extend_from_slice(payload);
        // offset 0 (< MIN_STUB_SIZE) even though offset+len fits.
        img.extend_from_slice(&write_footer(0, payload.len() as u64, AV, 0));
        assert_eq!(read_bundle_footer(&img), None);
    }

    #[test]
    fn bundle_magic_is_distinct_from_aso_magic() {
        assert_ne!(&BUNDLE_MAGIC[..4], &crate::vm::aso::ASO_MAGIC[..]);
        assert_eq!(&BUNDLE_MAGIC, b"ASCRIPTB");
    }

    #[test]
    fn footer_size_matches_layout() {
        // 8 + 8 + 4 + 2 + 2 + 8 = 32.
        assert_eq!(FOOTER_SIZE, 32);
        assert_eq!(write_footer(MIN_STUB_SIZE, 0, AV, 0).len(), FOOTER_SIZE);
    }

    /// RT §7.2 baseline pin (Task 0): the low-level `from_bytes` PARSER accepts any
    /// bundle_version and any flags value (only length + magic are checked by `from_bytes`
    /// — the strictness matrix lives in `validate_footer`, NOT the parser). This anchors
    /// that the wire DECODE stayed permissive; the field formerly named `reserved` is now
    /// `flags` at the same offset.
    #[test]
    fn shipped_reader_ignores_version_and_reserved() {
        let mut f = BundleFooter {
            payload_offset: MIN_STUB_SIZE,
            payload_len: 8,
            aso_version: AV,
            bundle_version: 99,
            flags: 0xFFFF,
            magic: BUNDLE_MAGIC,
        };
        assert!(
            BundleFooter::from_bytes(&f.to_bytes()).is_some(),
            "reader should accept bundle_version=99 flags=0xFFFF"
        );
        f.flags = 0;
        assert!(
            BundleFooter::from_bytes(&f.to_bytes()).is_some(),
            "reader should accept bundle_version=99 flags=0"
        );
    }

    /// RT §6.1 / §7.2 pin: a `flags=0` writer emits bundle_version=1, flags=0 — the
    /// pre-RT, bit-identical default that every shipped bundle carries.
    #[test]
    fn write_footer_emits_version1_reserved0() {
        let b = write_footer(MIN_STUB_SIZE, 4, AV, 0);
        let f = BundleFooter::from_bytes(&b).unwrap();
        assert_eq!(
            (f.bundle_version, f.flags),
            (BUNDLE_FOOTER_VERSION, 0),
            "write_footer(flags=0) must emit version={BUNDLE_FOOTER_VERSION} flags=0"
        );
        assert_eq!(BUNDLE_FOOTER_VERSION, 1, "BUNDLE_FOOTER_VERSION constant must be 1");
    }

    // ── RT §7.2 — codec tests (Step 1: written before the matrix existed; now green) ──

    /// `flags=0` ⇒ a footer BIT-IDENTICAL to the captured pre-RT golden 32 bytes. The
    /// golden is constructed from the live constants (so an `ASO_FORMAT_VERSION` bump moves
    /// it deterministically) AND independently version/flags-pinned to 1/0.
    #[test]
    fn flags_zero_footer_is_byte_identical_to_v1_golden() {
        let stub = MIN_STUB_SIZE;
        let payload_len: u64 = 4242;
        let got = write_footer(stub, payload_len, AV, 0);

        // The pre-RT v1 wire layout, hand-assembled: offset|len|aso_version|ver=1|flags=0|magic.
        let mut golden = [0u8; FOOTER_SIZE];
        golden[0..8].copy_from_slice(&stub.to_le_bytes());
        golden[8..16].copy_from_slice(&payload_len.to_le_bytes());
        golden[16..20].copy_from_slice(&AV.to_le_bytes());
        golden[20..22].copy_from_slice(&1u16.to_le_bytes()); // bundle_version
        golden[22..24].copy_from_slice(&0u16.to_le_bytes()); // flags (was `reserved`)
        golden[24..32].copy_from_slice(&BUNDLE_MAGIC);

        assert_eq!(got, golden, "flags=0 footer must be bit-identical to the v1 golden");
        // And cross-check the parsed view.
        let f = BundleFooter::from_bytes(&got).unwrap();
        assert_eq!((f.bundle_version, f.flags), (1, 0));
    }

    /// `flags=FLAG_ZSTD` ⇒ bundle_version is bumped to 2.
    #[test]
    fn flags_zstd_bumps_version_to_2() {
        let f = BundleFooter::from_bytes(&write_footer(MIN_STUB_SIZE, 8, AV, FLAG_ZSTD)).unwrap();
        assert_eq!(f.bundle_version, 2, "any flag forces version 2");
        assert_eq!(f.flags, FLAG_ZSTD);
    }

    /// Build a `FOOTER_SIZE` image with an explicit version/flags and run it through
    /// `validate_footer`, returning the verdict. Region is always in-bounds.
    fn check_with(version: u16, flags: u16) -> FooterCheck {
        let stub = MIN_STUB_SIZE;
        let payload: &[u8] = b"PAYLOAD!";
        let mut footer = BundleFooter {
            payload_offset: stub,
            payload_len: payload.len() as u64,
            aso_version: AV,
            bundle_version: version,
            flags,
            magic: BUNDLE_MAGIC,
        }
        .to_bytes();
        // Assemble the whole image so exe_len is consistent with the region bounds.
        let mut img = vec![0u8; stub as usize];
        img.extend_from_slice(payload);
        img.extend_from_slice(&footer);
        let exe_len = img.len() as u64;
        // re-extract the tail (footer unchanged) and validate.
        footer.copy_from_slice(&img[img.len() - FOOTER_SIZE..]);
        validate_footer(&footer, exe_len)
    }

    /// §7.2 row 1: v1 + flags=0 → accept (Bundle).
    #[test]
    fn matrix_v1_flags0_accepts() {
        match check_with(1, 0) {
            FooterCheck::Bundle { flags, .. } => assert_eq!(flags, 0),
            other => panic!("expected Bundle, got {other:?}"),
        }
    }

    /// §7.2 row 2: v1 + flags≠0 → REFUSE (corruption/tampering).
    #[test]
    fn matrix_v1_flags_nonzero_refused_as_tampering() {
        match check_with(1, FLAG_ZSTD) {
            FooterCheck::Refused(msg) => {
                assert!(
                    msg.contains("corrupt or tampered"),
                    "v1+flags must name corruption/tampering, got: {msg}"
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// §7.2 row 3: v2 + a KNOWN flag (FLAG_ZSTD) → accept (Bundle) when zstd is compiled in.
    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn matrix_v2_known_flag_accepts() {
        match check_with(2, FLAG_ZSTD) {
            FooterCheck::Bundle { flags, .. } => assert_eq!(flags, FLAG_ZSTD),
            other => panic!("expected Bundle, got {other:?}"),
        }
    }

    /// §7.2 row 3 (no-zstd build): v2 + FLAG_ZSTD on a stub without the codec → refuse with
    /// the "built without compressed-bundle support" message.
    #[cfg(not(feature = "bundle-zstd"))]
    #[test]
    fn matrix_v2_zstd_without_support_refused() {
        match check_with(2, FLAG_ZSTD) {
            FooterCheck::Refused(msg) => assert!(
                msg.contains("built without compressed-bundle support"),
                "expected the no-support refusal, got: {msg}"
            ),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// §7.2 row 4: v2 + an UNKNOWN flag bit (0x0002) → refuse "does not understand".
    #[test]
    fn matrix_v2_unknown_flag_refused() {
        match check_with(2, 0x0002) {
            FooterCheck::Refused(msg) => assert!(
                msg.contains("this bundle uses features this runtime does not understand"),
                "expected the unknown-feature refusal, got: {msg}"
            ),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    /// §7.2 row 5: version > 2 → refuse "built by a newer ascript".
    #[test]
    fn matrix_version_too_new_refused() {
        match check_with(3, 0) {
            FooterCheck::Refused(msg) => assert!(
                msg.contains("built by a newer ascript"),
                "expected the newer-ascript refusal, got: {msg}"
            ),
            other => panic!("expected Refused, got {other:?}"),
        }
        // ...even with flags set, the version check fires first.
        assert!(matches!(check_with(5, FLAG_ZSTD), FooterCheck::Refused(_)));
    }

    /// §7.2 last row: garbage / wrong-magic / truncated → NotABundle (unchanged
    /// fall-through, never a Refused).
    #[test]
    fn matrix_garbage_is_not_a_bundle() {
        // wrong magic
        let mut footer = write_footer(MIN_STUB_SIZE, 0, AV, 0);
        footer[24] ^= 0xFF;
        assert_eq!(
            validate_footer(&footer, MIN_STUB_SIZE + FOOTER_SIZE as u64),
            FooterCheck::NotABundle
        );
        // too short
        assert_eq!(validate_footer(&[0u8; 4], 4), FooterCheck::NotABundle);
        // valid magic but payload region past EOF (impossible region → NotABundle, not refuse)
        let bad = write_footer(MIN_STUB_SIZE, 1_000_000, AV, 0);
        assert_eq!(
            validate_footer(&bad, MIN_STUB_SIZE + FOOTER_SIZE as u64),
            FooterCheck::NotABundle
        );
    }

    // ── RT §7.3 — compressed-payload codec (security-critical) ──

    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn compress_decompress_round_trips() {
        let original: Vec<u8> = (0..50_000u32).flat_map(|i| (i % 7).to_le_bytes()).collect();
        let comp = compress_payload(&original).unwrap();
        // container = u64 len prefix || zstd frame
        assert_eq!(
            u64::from_le_bytes(comp[0..8].try_into().unwrap()),
            original.len() as u64
        );
        assert!(comp.len() < original.len(), "repetitive data must shrink");
        let back = decompress_payload(&comp).unwrap();
        assert_eq!(back, original);
    }

    /// SECURITY: `uncompressed_len` lied TOO HIGH (but within the cap) → length-mismatch
    /// error after a bounded decode, never a panic.
    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn decompress_len_lie_too_high_is_clean_error() {
        let original = b"the real payload bytes".to_vec();
        let mut comp = compress_payload(&original).unwrap();
        // overstate the declared length.
        comp[0..8].copy_from_slice(&((original.len() as u64) + 5).to_le_bytes());
        let err = decompress_payload(&comp).unwrap_err();
        assert!(err.contains("length mismatch"), "got: {err}");
    }

    /// SECURITY: `uncompressed_len` lied TOO LOW → the capped reader trips, length-mismatch
    /// error, NO unbounded growth.
    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn decompress_len_lie_too_low_is_clean_error() {
        let original = b"the real payload bytes, somewhat longer".to_vec();
        let mut comp = compress_payload(&original).unwrap();
        comp[0..8].copy_from_slice(&3u64.to_le_bytes()); // claim only 3 bytes
        let err = decompress_payload(&comp).unwrap_err();
        assert!(err.contains("length mismatch"), "got: {err}");
    }

    /// SECURITY (the headline §7.3 test): an ABSURD declared length is refused BEFORE any
    /// allocation — no OOM. A `Vec::with_capacity(declared)` here would abort the process;
    /// the cap check must fire first.
    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn decompress_absurd_len_refused_no_over_allocation() {
        let original = b"x".to_vec();
        let mut comp = compress_payload(&original).unwrap();
        comp[0..8].copy_from_slice(&u64::MAX.to_le_bytes()); // 16 EiB
        let err = decompress_payload(&comp).unwrap_err();
        assert!(
            err.contains("implausible uncompressed size") && err.contains("refusing"),
            "an absurd length must be refused pre-allocation, got: {err}"
        );
        // Just over the cap is also refused.
        comp[0..8].copy_from_slice(&(MAX_UNCOMPRESSED_PAYLOAD + 1).to_le_bytes());
        assert!(decompress_payload(&comp)
            .unwrap_err()
            .contains("implausible"));
    }

    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn decompress_truncated_prefix_is_clean_error() {
        assert!(decompress_payload(&[1, 2, 3]).unwrap_err().contains("truncated"));
    }

    #[cfg(feature = "bundle-zstd")]
    #[test]
    fn decompress_garbage_frame_is_clean_error() {
        let mut data = Vec::new();
        data.extend_from_slice(&10u64.to_le_bytes());
        data.extend_from_slice(b"not a zstd frame at all");
        let err = decompress_payload(&data).unwrap_err();
        // Either "not a valid zstd frame" (header) or a decompress error — both clean.
        assert!(
            err.contains("zstd") || err.contains("decompress"),
            "got: {err}"
        );
    }
}
