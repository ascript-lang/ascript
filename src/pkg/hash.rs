//! The `asum1` normalized-tree content hash (SP6 §5.3) — fail-closed integrity.
//!
//! Hash a NORMALIZED file manifest, not a tarball, so the digest is stable across
//! OSes, re-clones, and archive formats:
//!
//! 1. Walk the package root; collect every file ending in `.as` PLUS the
//!    package's `ascript.toml`. Exclude `.aso`, VCS dirs (`.git`), and any
//!    leading-dot dirs (editor/cache cruft).
//! 2. For each file: `(relative-path-as-/-joined-utf8, sha256(file-bytes))`.
//! 3. Sort the pairs by relative path (byte order).
//! 4. Outer sha256: for each pair write `len(path) as u64-le || path || digest`
//!    (length-prefixed → no delimiter ambiguity).
//! 5. Result = `asum1-` + base64url(no-padding) of the 32-byte digest.
//!
//! File bytes are hashed VERBATIM (no line-ending normalization). The `asum1-`
//! prefix versions the algorithm (rotate to `asum2-` for sha3/blake3 later).

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::Path;

/// The algorithm-versioning prefix on every emitted hash.
pub const PREFIX: &str = "asum1-";

/// Compute the `asum1` hash of the package tree rooted at `root`.
///
/// Returns an `asum1-<base64url>` string. An IO error reading the tree (e.g. an
/// unreadable file) is surfaced as a clear error string — the hash is
/// fail-closed, so a tree we cannot fully read is not silently hashed.
pub fn asum1_tree(root: &Path) -> Result<String, String> {
    let mut entries: Vec<(String, [u8; 32])> = Vec::new();
    collect(root, root, &mut entries)?;
    // Stable order: sort by the normalized relative path (byte order).
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut outer = Sha256::new();
    for (path, digest) in &entries {
        let bytes = path.as_bytes();
        outer.update((bytes.len() as u64).to_le_bytes());
        outer.update(bytes);
        outer.update(digest);
    }
    let digest = outer.finalize();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Ok(format!("{PREFIX}{b64}"))
}

/// Recursively collect `(relative-/-path, sha256)` for every included file.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, [u8; 32])>) -> Result<(), String> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {} for hashing: {e}", dir.display()))?;
    for entry in read {
        let entry = entry.map_err(|e| format!("cannot read directory entry: {e}"))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        let file_type = entry
            .file_type()
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;

        if file_type.is_dir() {
            // Exclude any leading-dot directory (`.git`, `.cache`, editor dirs).
            if name.starts_with('.') {
                continue;
            }
            collect(root, &path, out)?;
            continue;
        }

        // Symlinks are not followed (avoid escaping the tree / loops); only
        // regular files contribute to the hash.
        if !file_type.is_file() {
            continue;
        }

        if !is_included(&name) {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("path {} escaped the package root", path.display()))?;
        let rel_norm = normalize_rel(rel);

        let bytes = std::fs::read(&path)
            .map_err(|e| format!("cannot read {} for hashing: {e}", path.display()))?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        out.push((rel_norm, digest));
    }
    Ok(())
}

/// A file is included iff it ends in `.as` or is exactly `ascript.toml`. The
/// compiled `.aso` cache is deliberately excluded (the source is the contract).
fn is_included(name: &str) -> bool {
    name == "ascript.toml" || (name.ends_with(".as") && !name.ends_with(".aso"))
}

/// Normalize a relative path to forward-slash-joined UTF-8 (stable across OSes).
fn normalize_rel(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "asum1-test-{}-{}-{:?}",
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

    fn write(dir: &Path, rel: &str, contents: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    #[test]
    fn stable_across_calls() {
        let d = scratch("stable");
        write(&d, "main.as", "print(1)\n");
        write(&d, "ascript.toml", "[package]\nname=\"x\"\nversion=\"1.0.0\"\n");
        let a = asum1_tree(&d).unwrap();
        let b = asum1_tree(&d).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("asum1-"), "{a}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn output_round_trips_base64url() {
        let d = scratch("b64");
        write(&d, "a.as", "x");
        let h = asum1_tree(&d).unwrap();
        let body = h.strip_prefix("asum1-").unwrap();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .expect("valid base64url");
        assert_eq!(bytes.len(), 32, "sha256 is 32 bytes");
        // base64url uses `-`/`_`, never `+`/`/`/`=`.
        assert!(!body.contains('+') && !body.contains('/') && !body.contains('='), "{body}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn order_independent() {
        // Two trees with the same files but written in different enumeration
        // orders must hash identically (we sort).
        let d1 = scratch("ord1");
        write(&d1, "a.as", "AAA");
        write(&d1, "b.as", "BBB");
        write(&d1, "sub/c.as", "CCC");
        let d2 = scratch("ord2");
        write(&d2, "sub/c.as", "CCC");
        write(&d2, "b.as", "BBB");
        write(&d2, "a.as", "AAA");
        assert_eq!(asum1_tree(&d1).unwrap(), asum1_tree(&d2).unwrap());
        let _ = fs::remove_dir_all(&d1);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn changes_when_as_content_changes() {
        let d = scratch("change");
        write(&d, "main.as", "print(1)\n");
        let before = asum1_tree(&d).unwrap();
        write(&d, "main.as", "print(2)\n");
        let after = asum1_tree(&d).unwrap();
        assert_ne!(before, after);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn ignores_aso_and_git_and_dotdirs() {
        let d = scratch("ignore");
        write(&d, "main.as", "print(1)\n");
        let baseline = asum1_tree(&d).unwrap();

        // A sibling .aso must NOT change the hash.
        write(&d, "main.aso", "compiled-bytes-ignored");
        assert_eq!(asum1_tree(&d).unwrap(), baseline, ".aso must be ignored");

        // A .git/ dir must NOT change the hash.
        write(&d, ".git/config", "[core]\n");
        write(&d, ".git/objects/ab/cdef", "blob");
        assert_eq!(asum1_tree(&d).unwrap(), baseline, ".git must be ignored");

        // Some other leading-dot dir is also ignored.
        write(&d, ".vscode/settings.json", "{}");
        assert_eq!(asum1_tree(&d).unwrap(), baseline, "dot-dirs ignored");

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_changes_hash() {
        // Path is part of the hashed manifest, so renaming a file changes it.
        let d1 = scratch("ren1");
        write(&d1, "a.as", "X");
        let d2 = scratch("ren2");
        write(&d2, "b.as", "X");
        assert_ne!(asum1_tree(&d1).unwrap(), asum1_tree(&d2).unwrap());
        let _ = fs::remove_dir_all(&d1);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn toml_is_part_of_the_tree() {
        let d = scratch("toml");
        write(&d, "main.as", "print(1)\n");
        let before = asum1_tree(&d).unwrap();
        write(&d, "ascript.toml", "[package]\nname=\"x\"\nversion=\"1.0.0\"\n");
        assert_ne!(asum1_tree(&d).unwrap(), before, "ascript.toml is hashed");
        let _ = fs::remove_dir_all(&d);
    }
}
