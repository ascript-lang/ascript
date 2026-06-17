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
/// `ASO_FORMAT_VERSION`, which versions the embedded payload).
pub const BUNDLE_FOOTER_VERSION: u16 = 1;

/// A conservative floor on the stub (runtime copy) size. The real `ascript` runtime is tens
/// of MB; a `payload_offset` below this is structurally impossible and is rejected (defends
/// against a footer that points the payload into the very start of the image).
pub const MIN_STUB_SIZE: u64 = 4096;

/// The fixed footer size in bytes: `payload_offset(8) + payload_len(8) + aso_version(4) +
/// bundle_version(2) + reserved(2) + magic(8)`.
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
    /// The footer layout version ([`BUNDLE_FOOTER_VERSION`]).
    pub bundle_version: u16,
    /// Reserved (zero) — future flags without a layout bump.
    pub reserved: u16,
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
        b[22..24].copy_from_slice(&self.reserved.to_le_bytes());
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
            reserved: u16::from_le_bytes(buf[22..24].try_into().ok()?),
            magic,
        })
    }
}

/// Build the footer for a bundle whose stub is `stub_len` bytes and whose payload is
/// `payload_len` bytes (the payload sits at offset `stub_len`, immediately after the stub).
/// `aso_version` is the embedded payload's `ASO_FORMAT_VERSION` (informational).
pub fn write_footer(stub_len: u64, payload_len: u64, aso_version: u32) -> [u8; FOOTER_SIZE] {
    BundleFooter {
        payload_offset: stub_len,
        payload_len,
        aso_version,
        bundle_version: BUNDLE_FOOTER_VERSION,
        reserved: 0,
        magic: BUNDLE_MAGIC,
    }
    .to_bytes()
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
    validate_footer(&exe_bytes[exe_len - FOOTER_SIZE..], exe_len as u64)
}

/// Validate a `FOOTER_SIZE`-byte footer tail against the TOTAL image length `exe_len`,
/// returning the bounds-checked `(payload_offset, payload_len)` or `None`. This is the core
/// of [`read_bundle_footer`]; the startup shim ([`crate::try_run_embedded`]) uses it directly
/// so it can validate from just the file length + a small tail read, WITHOUT loading the
/// whole (tens-of-MB) executable on every launch (Task 7 startup budget). NEVER panics.
pub fn validate_footer(footer_tail: &[u8], exe_len: u64) -> Option<(usize, usize)> {
    if exe_len < FOOTER_SIZE as u64 {
        return None;
    }
    let footer = BundleFooter::from_bytes(footer_tail)?;
    if footer.payload_offset < MIN_STUB_SIZE {
        return None;
    }
    // The region available for `stub || payload` is everything before the footer.
    let region = exe_len - FOOTER_SIZE as u64;
    // checked_add: a crafted huge offset/len must not wrap into a passing range.
    let end = footer.payload_offset.checked_add(footer.payload_len)?;
    if end > region {
        return None;
    }
    // Both fit in usize on a 64-bit host (end <= region < exe_len).
    Some((footer.payload_offset as usize, footer.payload_len as usize))
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
        v.extend_from_slice(&write_footer(stub_len as u64, payload.len() as u64, AV));
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
            reserved: 0,
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
        img.extend_from_slice(&write_footer(stub_len, 1_000_000, AV));
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
            reserved: 0,
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
        img.extend_from_slice(&write_footer(0, payload.len() as u64, AV));
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
        assert_eq!(write_footer(MIN_STUB_SIZE, 0, AV).len(), FOOTER_SIZE);
    }

    /// RT §7.2 baseline pin: the SHIPPED reader accepts any bundle_version and any
    /// reserved value (only length + magic are checked by `from_bytes`).
    #[test]
    fn shipped_reader_ignores_version_and_reserved() {
        let mut f = BundleFooter {
            payload_offset: MIN_STUB_SIZE,
            payload_len: 8,
            aso_version: AV,
            bundle_version: 99,
            reserved: 0xFFFF,
            magic: BUNDLE_MAGIC,
        };
        assert!(
            BundleFooter::from_bytes(&f.to_bytes()).is_some(),
            "reader should accept bundle_version=99 reserved=0xFFFF"
        );
        f.reserved = 0;
        assert!(
            BundleFooter::from_bytes(&f.to_bytes()).is_some(),
            "reader should accept bundle_version=99 reserved=0"
        );
    }

    /// RT §6.1: writers have only ever emitted bundle_version=1, reserved=0.
    #[test]
    fn write_footer_emits_version1_reserved0() {
        let b = write_footer(MIN_STUB_SIZE, 4, AV);
        let f = BundleFooter::from_bytes(&b).unwrap();
        assert_eq!(
            (f.bundle_version, f.reserved),
            (BUNDLE_FOOTER_VERSION, 0),
            "write_footer must emit version={BUNDLE_FOOTER_VERSION} reserved=0"
        );
        assert_eq!(BUNDLE_FOOTER_VERSION, 1, "BUNDLE_FOOTER_VERSION constant must be 1");
    }
}
