//! RT §8 — `--oci`: deterministic OCI image tarball writer (no Docker at build time).
//!
//! `ascript build --oci app.as -o app.tar` writes one OCI Image Layout tarball:
//!
//! ```text
//! oci-layout                      {"imageLayoutVersion":"1.0.0"}
//! index.json                      image index
//! blobs/sha256/<manifest-digest>  application/vnd.oci.image.manifest.v1+json
//! blobs/sha256/<config-digest>    application/vnd.oci.image.config.v1+json
//! blobs/sha256/<layer-digest>     application/vnd.oci.image.layer.v1.tar+gzip
//! ```
//!
//! All JSON is compact with hand-ordered keys (serde_json `preserve_order` is on)
//! so digests are deterministic. The inner tar (holding `/app`) and the outer tar
//! (the OCI layout archive) are written by a hand-rolled USTAR writer — no `tar`
//! crate dep needed for a single fixed-name file; deterministic by construction
//! (fixed timestamps, sorted entries, uid/gid 0, pinned gzip level).
//!
//! **Owner decision (recorded here and in spec §8.1):** RT ships its own bounded
//! 1-entry inner-tar writer rather than taking an additional `tar` dep. Unifying
//! onto BATT's future `tarcore` is an OPTIONAL later refactor, not a dependency.
//!
//! **Feature gate decision (spec §8.3 / plan Task 9 Step 3):** `--oci` is gated on
//! `cfg(feature = "compress")` — flate2 (already in `compress`) provides gzip.
//! The toolchain default has `compress`; `--no-default-features` gets a clean
//! "rebuild with compress support" error. sha2 is a CORE (non-optional) dep; only
//! serde_json requires a note — it is behind the `data` feature (also in default);
//! since we require `compress` for gzip anyway, we document the gate as `compress`
//! and rely on the default feature set which has both. RECORDED in spec status header.
//!
//! Gated: entire file under `#[cfg(feature = "compress")]`.

#![cfg(feature = "compress")]

use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::Path;

// ── Timestamp ──────────────────────────────────────────────────────────────

/// Fixed OCI `created` timestamp (§9.1): epoch zero unless `SOURCE_DATE_EPOCH` is
/// set (the reproducible-builds convention). Double-build with the same
/// `SOURCE_DATE_EPOCH` is byte-identical.
fn created_timestamp() -> String {
    if let Ok(sde) = std::env::var("SOURCE_DATE_EPOCH") {
        if let Ok(secs) = sde.trim().parse::<u64>() {
            return secs_to_rfc3339(secs);
        }
    }
    "1970-01-01T00:00:00Z".to_string()
}

/// Convert UNIX seconds to `YYYY-MM-DDTHH:MM:SSZ` (UTC, no sub-second).
/// Hand-rolled Gregorian arithmetic — no chrono dep required.
fn secs_to_rfc3339(secs: u64) -> String {
    let secs_per_day: u64 = 86400;
    let days = secs / secs_per_day;
    let tod = secs % secs_per_day;
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;

    // Julian-Day-Number → Gregorian (Hatcher 1984 / civil.rs algorithm).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Pinned gzip compression level (§9.1 determinism contract).
const GZIP_LEVEL: u32 = 6;

// ── sha256 helpers ─────────────────────────────────────────────────────────

fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex32(d: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in d {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ── USTAR tar writer (hand-rolled, ~100 lines, §8.1 owner decision) ────────
//
// Produces POSIX USTAR tars. Each entry:
//   - 512-byte header block
//   - data blocks (padded to 512-byte boundary)
// End-of-archive: two 512-byte zero blocks.

/// Build one USTAR 512-byte header for a regular file.
fn ustar_header(name: &[u8], size: u64, mtime: u64, mode: u32) -> [u8; 512] {
    let mut hdr = [0u8; 512];

    // Name (bytes 0..100, null-padded, max 100 chars).
    let name_len = name.len().min(100);
    hdr[..name_len].copy_from_slice(&name[..name_len]);

    // Mode (bytes 100..108), UID (108..116), GID (116..124).
    write_octal(&mut hdr[100..108], mode as u64);
    write_octal(&mut hdr[108..116], 0); // uid 0 (root)
    write_octal(&mut hdr[116..124], 0); // gid 0 (root)

    // Size (bytes 124..136), mtime (136..148).
    write_octal(&mut hdr[124..136], size);
    write_octal(&mut hdr[136..148], mtime);

    // Checksum placeholder (bytes 148..156) = spaces (required by standard).
    for b in &mut hdr[148..156] {
        *b = b' ';
    }

    // Type flag (byte 156): '0' = regular file.
    hdr[156] = b'0';

    // Magic + version (bytes 257..265): "ustar\000".
    hdr[257..263].copy_from_slice(b"ustar\0");
    hdr[263..265].copy_from_slice(b"00");

    // Compute checksum and write it (bytes 148..156).
    let cksum: u64 = hdr.iter().map(|&b| b as u64).sum();
    write_octal_cksum(&mut hdr[148..156], cksum);

    hdr
}

/// NUL-terminated octal string, exactly `field.len()` bytes.
fn write_octal(field: &mut [u8], value: u64) {
    let s = format!("{:0>width$o}\0", value, width = field.len() - 1);
    field.copy_from_slice(&s.as_bytes()[..field.len()]);
}

/// USTAR checksum: 6 octal digits + NUL + space.
fn write_octal_cksum(field: &mut [u8], cksum: u64) {
    let s = format!("{cksum:06o}\0 ");
    let len = field.len().min(s.len());
    field[..len].copy_from_slice(&s.as_bytes()[..len]);
}

/// Round `n` up to the next multiple of 512.
fn pad512(n: usize) -> usize {
    (n + 511) & !511
}

/// Build a deterministic single-entry USTAR tar.
///
/// `name_in_tar`: the entry name (e.g. `b"app"`).
/// `content`: file bytes.
/// `mtime`: seconds since epoch (0 for determinism).
/// `mode`: Unix permission bits (e.g. 0o755).
///
/// Returns the complete tar bytes (header + data + end-of-archive).
pub fn build_single_entry_tar(
    name_in_tar: &[u8],
    content: &[u8],
    mtime: u64,
    mode: u32,
) -> Vec<u8> {
    let size = content.len();
    let hdr = ustar_header(name_in_tar, size as u64, mtime, mode);
    let padded = pad512(size);
    let mut out = Vec::with_capacity(512 + padded + 1024);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(content);
    out.extend(std::iter::repeat_n(0u8, padded - size)); // padding
    out.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive
    out
}

/// Gzip-compress with `flate2::GzBuilder`, mtime 0, pinned level (deterministic).
fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::write::GzEncoder;
    use flate2::{Compression, GzBuilder};

    let enc = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::new(GZIP_LEVEL));
    let mut enc: GzEncoder<Vec<u8>> = enc;
    enc.write_all(data)
        .map_err(|e| format!("gzip write: {e}"))?;
    enc.finish().map_err(|e| format!("gzip finish: {e}"))
}

// ── Outer OCI tar ─────────────────────────────────────────────────────────

/// Append one entry to an outer tar buffer.
fn tar_append(buf: &mut Vec<u8>, name: &[u8], content: &[u8], mtime: u64) {
    let hdr = ustar_header(name, content.len() as u64, mtime, 0o644);
    buf.extend_from_slice(&hdr);
    buf.extend_from_slice(content);
    let pad = pad512(content.len()) - content.len();
    buf.extend(std::iter::repeat_n(0u8, pad));
}

// ── Architecture mapping ───────────────────────────────────────────────────

/// Map a Rust triple to an OCI architecture string (§8.1 / §8.3).
/// Returns `Err` with a clear rejection message for non-musl targets.
pub fn oci_arch_from_triple(triple: &str) -> Result<&'static str, String> {
    match triple {
        "x86_64-unknown-linux-musl" => Ok("amd64"),
        "aarch64-unknown-linux-musl" => Ok("arm64"),
        other => Err(oci_target_rejection_message(other)),
    }
}

/// A clear rejection message naming the musl equivalent (§8.3).
pub fn oci_target_rejection_message(triple: &str) -> String {
    let suggestion = if triple.contains("x86_64") {
        "x86_64-unknown-linux-musl"
    } else if triple.contains("aarch64") || triple.contains("arm64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl or aarch64-unknown-linux-musl"
    };
    format!(
        "--oci requires a *-unknown-linux-musl target (scratch-base images must be \
         statically linked); '{triple}' is not a musl target — use \
         --target {suggestion} instead, or omit --target to default to \
         <host-arch>-unknown-linux-musl"
    )
}

// ── JSON builders (compact, insertion-ordered keys) ────────────────────────

fn build_config_json(arch: &str, created: &str, diff_id_hex: &str) -> String {
    use serde_json::{json, Value};
    let v: Value = json!({
        "created": created,
        "architecture": arch,
        "os": "linux",
        "config": { "Entrypoint": ["/app"] },
        "rootfs": {
            "type": "layers",
            "diff_ids": [ format!("sha256:{diff_id_hex}") ]
        },
        "history": [{ "created": created, "created_by": "ascript build --oci" }]
    });
    serde_json::to_string(&v).expect("infallible")
}

fn build_manifest_json(
    config_digest: &str,
    config_size: usize,
    layer_digest: &str,
    layer_size: usize,
) -> String {
    use serde_json::json;
    let v = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{config_digest}"),
            "size": config_size
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{layer_digest}"),
            "size": layer_size
        }]
    });
    serde_json::to_string(&v).expect("infallible")
}

fn build_index_json(manifest_digest: &str, manifest_size: usize, arch: &str, tag: &str) -> String {
    use serde_json::json;
    let v = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{manifest_digest}"),
            "size": manifest_size,
            "platform": { "architecture": arch, "os": "linux" },
            "annotations": {
                "org.opencontainers.image.ref.name": tag
            }
        }]
    });
    serde_json::to_string(&v).expect("infallible")
}

// ── Public entry point ─────────────────────────────────────────────────────

/// Build and write an OCI Image Layout tar from `bundle_bytes`.
///
/// `bundle_bytes` is the self-contained binary (the output of `build_native`).
/// `arch` is the OCI architecture string (`"amd64"` / `"arm64"`).
/// `tag` is the image reference annotation (e.g. `"app:latest"`).
/// `output_path` is where to write the `.tar` file (atomically via temp-rename).
///
/// Everything is deterministic: timestamps from `SOURCE_DATE_EPOCH` or epoch-zero,
/// sorted tar entries, pinned gzip level, sha-256 digests.
pub fn write_oci_tar(
    bundle_bytes: &[u8],
    arch: &str,
    tag: &str,
    output_path: &Path,
) -> Result<(), String> {
    let created = created_timestamp();

    // ── Inner tar → gzip (the layer) ──────────────────────────────────────
    // Inner tar: exactly `/app`, mode 0755, uid/gid 0, mtime 0 (§9.1).
    let inner_tar = build_single_entry_tar(b"app", bundle_bytes, 0, 0o755);

    // diff_id = sha256(UNCOMPRESSED inner tar) — two-digest rule (§8.1).
    let diff_id_bytes: [u8; 32] = sha256_of(&inner_tar);
    let diff_id_hex = hex32(&diff_id_bytes);

    let layer_gz = gzip_compress(&inner_tar)?;

    // Layer descriptor digest = sha256(GZIPPED bytes) — must differ from diff_id.
    let layer_digest_bytes: [u8; 32] = sha256_of(&layer_gz);
    let layer_digest_hex = hex32(&layer_digest_bytes);
    let layer_size = layer_gz.len();

    // ── Config ─────────────────────────────────────────────────────────────
    let config_json = build_config_json(arch, &created, &diff_id_hex);
    let config_bytes = config_json.as_bytes();
    let config_digest_hex = hex32(&sha256_of(config_bytes));
    let config_size = config_bytes.len();

    // ── Manifest ───────────────────────────────────────────────────────────
    let manifest_json =
        build_manifest_json(&config_digest_hex, config_size, &layer_digest_hex, layer_size);
    let manifest_bytes = manifest_json.as_bytes();
    let manifest_digest_hex = hex32(&sha256_of(manifest_bytes));
    let manifest_size = manifest_bytes.len();

    // ── index.json ─────────────────────────────────────────────────────────
    let index_json = build_index_json(&manifest_digest_hex, manifest_size, arch, tag);
    let index_bytes = index_json.as_bytes();

    // ── oci-layout ─────────────────────────────────────────────────────────
    const OCI_LAYOUT: &[u8] = b"{\"imageLayoutVersion\":\"1.0.0\"}";

    // ── Outer tar (sorted: oci-layout, index.json, blobs/…) ───────────────
    let outer_mtime: u64 = 0;
    let mut outer = Vec::new();
    tar_append(&mut outer, b"oci-layout", OCI_LAYOUT, outer_mtime);
    tar_append(&mut outer, b"index.json", index_bytes, outer_mtime);
    tar_append(
        &mut outer,
        format!("blobs/sha256/{manifest_digest_hex}").as_bytes(),
        manifest_bytes,
        outer_mtime,
    );
    tar_append(
        &mut outer,
        format!("blobs/sha256/{config_digest_hex}").as_bytes(),
        config_bytes,
        outer_mtime,
    );
    tar_append(
        &mut outer,
        format!("blobs/sha256/{layer_digest_hex}").as_bytes(),
        &layer_gz,
        outer_mtime,
    );
    // End-of-archive: two 512-byte zero blocks.
    outer.extend(std::iter::repeat_n(0u8, 1024));

    // ── Atomic write (temp-rename) ─────────────────────────────────────────
    let tmp = {
        let mut p = output_path.to_path_buf();
        let ext = p
            .extension()
            .map(|e| format!("{}.{}.tmp", e.to_string_lossy(), std::process::id()))
            .unwrap_or_else(|| format!("{}.tmp", std::process::id()));
        p.set_extension(ext);
        p
    };
    std::fs::write(&tmp, &outer)
        .map_err(|e| format!("cannot write OCI tar to {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, output_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!(
            "cannot rename {} -> {}: {e}",
            tmp.display(),
            output_path.display()
        )
    })?;

    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn epoch_zero_is_unix_epoch() {
        assert_eq!(secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_epoch_value() {
        // 1700000000 = 2023-11-14T22:13:20Z (verified with `date -d @1700000000 -u`).
        assert_eq!(secs_to_rfc3339(1700000000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn ustar_checksum_valid() {
        let hdr = ustar_header(b"app", 42, 0, 0o755);
        // Recompute with checksum field treated as spaces.
        let mut h2 = hdr;
        for b in &mut h2[148..156] {
            *b = b' ';
        }
        let expected: u64 = h2.iter().map(|&b| b as u64).sum();
        let stored_str = std::str::from_utf8(&hdr[148..154])
            .unwrap()
            .trim_end_matches('\0')
            .trim();
        let stored = u64::from_str_radix(stored_str, 8).unwrap();
        assert_eq!(stored, expected);
    }

    #[test]
    fn single_entry_tar_structure() {
        let tar = build_single_entry_tar(b"app", b"hello", 0, 0o755);
        // 512 header + 512 data block (5 bytes, padded to 512) + 1024 EOA = 2048
        assert_eq!(tar.len(), 2048);
        assert_eq!(&tar[..3], b"app");
        assert_eq!(&tar[512..517], b"hello");
        assert!(tar[1024..].iter().all(|&b| b == 0));
    }

    #[test]
    fn two_digest_rule() {
        let inner = build_single_entry_tar(b"app", b"binary-content", 0, 0o755);
        let diff_id: [u8; 32] = sha256_of(&inner);
        let gz = gzip_compress(&inner).unwrap();
        let layer_dig: [u8; 32] = sha256_of(&gz);
        assert_ne!(diff_id, layer_dig, "two-digest rule: diff_id must ≠ layer_digest");
    }

    #[test]
    fn gzip_is_deterministic() {
        let a = gzip_compress(b"test data 12345").unwrap();
        let b = gzip_compress(b"test data 12345").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn oci_arch_musl_mapping() {
        assert_eq!(oci_arch_from_triple("x86_64-unknown-linux-musl"), Ok("amd64"));
        assert_eq!(oci_arch_from_triple("aarch64-unknown-linux-musl"), Ok("arm64"));
    }

    #[test]
    fn oci_arch_gnu_rejected() {
        let r = oci_arch_from_triple("x86_64-unknown-linux-gnu");
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("musl"), "{msg}");
        assert!(msg.contains("x86_64-unknown-linux-musl"), "{msg}");
    }

    #[test]
    fn oci_arch_darwin_rejected() {
        let r = oci_arch_from_triple("aarch64-apple-darwin");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("musl"));
    }
}
