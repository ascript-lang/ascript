//! RT §5.3 — the content-addressed stub cache (pkg-store hygiene, reused).
//!
//! ```text
//! $ASCRIPT_CACHE/rt/sha256-<hex>/ascript-rt[.exe]   // immutable, content-addressed
//! $ASCRIPT_CACHE/tmp/                                // the existing SP6 staging dir
//! ```
//!
//! - **Atomic publish:** stage into `tmp/`, write, `chmod +x` (unix), then a single
//!   `rename` into `rt/sha256-<hex>/`. A failed rename (read-only slot) is a clean
//!   `Err` with NO partial state left in `tmp/`.
//! - **Verify-on-load:** every cache hit is RE-HASHED before use; a mismatch (bit-rot,
//!   tamper) deletes the entry and returns `None`. A cached stub is NEVER trusted by
//!   path.
//!
//! This module compiles WITHOUT the `rt-fetch` feature (the `--no-default-features`
//! build still has a working cache). It depends only on `std` + `sha2` (a core dep).
//!
//! The cache ROOT is resolved exactly like the SP6 package store (`$ASCRIPT_CACHE`
//! first, then the per-OS default, then a tempdir last resort — deliberately no `dirs`
//! crate), so an `--exact` publish and a fetch publish land under the SAME root.
//!
//! **This [`cache_root`] is the ONE canonical implementation** (RT T6 nit). The binary
//! `pkg::cache::cache_root` is now a thin delegate to it (the binary depends on the
//! library, so it can call in — there is no longer a replicated copy to drift). Both the
//! package store and the rt stub cache therefore resolve to the SAME root by
//! construction.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Resolve the cache root, honoring `$ASCRIPT_CACHE` first, then the per-OS default,
/// then a tempdir last resort. The ONE canonical implementation: `pkg::cache::cache_root`
/// delegates here (see the module note). Never fails.
pub fn cache_root() -> PathBuf {
    if let Some(p) = std::env::var_os("ASCRIPT_CACHE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    platform_default()
}

fn nonempty_env(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|v| !v.is_empty())
}

#[cfg(target_os = "macos")]
fn platform_default() -> PathBuf {
    if let Some(xdg) = nonempty_env("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("ascript");
    }
    if let Some(home) = nonempty_env("HOME") {
        return PathBuf::from(home).join("Library/Caches/ascript");
    }
    temp_fallback()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_default() -> PathBuf {
    if let Some(xdg) = nonempty_env("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("ascript");
    }
    if let Some(home) = nonempty_env("HOME") {
        return PathBuf::from(home).join(".cache/ascript");
    }
    temp_fallback()
}

#[cfg(windows)]
fn platform_default() -> PathBuf {
    if let Some(local) = nonempty_env("LOCALAPPDATA") {
        return PathBuf::from(local).join("ascript").join("Cache");
    }
    temp_fallback()
}

#[cfg(not(any(unix, windows)))]
fn platform_default() -> PathBuf {
    let _ = nonempty_env;
    temp_fallback()
}

fn temp_fallback() -> PathBuf {
    std::env::temp_dir().join("ascript-cache")
}

/// The staging dir used during fetch+hash before the atomic move into `rt/` (the same
/// `$ASCRIPT_CACHE/tmp/` dir the SP6 store stages through).
pub fn tmp_dir() -> PathBuf {
    cache_root().join("tmp")
}

/// The on-disk filename for the cached stub binary (`.exe` on Windows).
pub fn stub_filename() -> &'static str {
    if cfg!(windows) {
        "ascript-rt.exe"
    } else {
        "ascript-rt"
    }
}

/// The content-addressed slot directory for a stub whose blob sha256 is `sha256_hex`.
pub fn slot_dir(sha256_hex: &str) -> PathBuf {
    cache_root().join("rt").join(format!("sha256-{sha256_hex}"))
}

/// The full path to the cached stub binary for `sha256_hex`.
pub fn stub_path(sha256_hex: &str) -> PathBuf {
    slot_dir(sha256_hex).join(stub_filename())
}

/// Lowercase-hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Publish `bytes` to the content-addressed cache under `rt/sha256-<hex>/`.
///
/// `expected_sha` is the manifest-pinned digest. **Before publishing, the bytes are
/// re-hashed and MUST equal `expected_sha`** (defense in depth — a caller that pins the
/// wrong digest cannot land mismatched bytes in the content-addressed store). The write
/// is atomic: stage into `tmp/`, `chmod +x` (unix), then a single `rename` into the
/// slot. A failed rename surfaces a clean `Err` and removes the staging file — NO
/// partial state remains.
pub fn publish(bytes: &[u8], expected_sha: &str) -> Result<PathBuf, String> {
    // Defense in depth: the published bytes must match the pin.
    let actual = sha256_hex(bytes);
    if actual != expected_sha {
        return Err(format!(
            "refusing to publish: bytes hash to sha256 {actual} but the pin is {expected_sha}"
        ));
    }

    let tmp = tmp_dir();
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create cache tmp dir: {e}"))?;

    // A unique staging filename so two concurrent publishes never collide.
    let staged = tmp.join(format!(
        "rt-stub-{}-{}.staging",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    // Write, set the executable bit, then rename. Any failure cleans the staged file.
    let result = (|| -> Result<PathBuf, String> {
        std::fs::write(&staged, bytes)
            .map_err(|e| format!("cannot stage stub in cache: {e}"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&staged)
                .map_err(|e| format!("cannot stat staged stub: {e}"))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&staged, perms)
                .map_err(|e| format!("cannot chmod +x the staged stub: {e}"))?;
        }

        let slot = slot_dir(expected_sha);
        std::fs::create_dir_all(&slot)
            .map_err(|e| format!("cannot create cache slot dir: {e}"))?;
        let dest = slot.join(stub_filename());

        std::fs::rename(&staged, &dest)
            .map_err(|e| format!("cannot publish stub into cache slot: {e}"))?;
        Ok(dest)
    })();

    // On ANY failure, remove the staging file so no partial state remains in tmp/.
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Load a cached stub by its content digest, RE-HASHING the file before trusting it.
///
/// A cache hit is never trusted by path: the file is read and its sha256 compared to
/// `sha256_hex`. On a mismatch (bit-rot, tamper, partial write) the entry is DELETED and
/// `None` is returned so the caller falls through to a fresh fetch. Returns `Some(path)`
/// only when the on-disk bytes verify.
pub fn load(sha256_hex: &str) -> Option<PathBuf> {
    let path = stub_path(sha256_hex);
    let bytes = std::fs::read(&path).ok()?;
    let actual = self::sha256_hex(&bytes);
    if actual == sha256_hex {
        Some(path)
    } else {
        // Verify-on-load failed: evict the whole slot so a refetch republishes cleanly.
        let _ = std::fs::remove_dir_all(slot_dir(sha256_hex));
        None
    }
}

/// A process-wide lock serializing tests that mutate the global `$ASCRIPT_CACHE` env
/// var. Public (not `#[cfg(test)]`) because the `tests/rt_supply_chain.rs` INTEGRATION
/// test links the crate's normal build and cannot reach a `cfg(test)` item. It guards
/// only test-time env mutation; production code never touches it.
#[doc(hidden)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static RT_TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    RT_TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Pin `$ASCRIPT_CACHE` to a fresh tempdir for the body, under the env lock.
    fn with_cache<R>(tag: &str, body: impl FnOnce(&Path) -> R) -> R {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "rt-cache-unit-{}-{}-{:?}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", &dir);
        let r = body(&dir);
        match prev {
            Some(v) => std::env::set_var("ASCRIPT_CACHE", v),
            None => std::env::remove_var("ASCRIPT_CACHE"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    #[test]
    fn publish_then_load_happy() {
        with_cache("happy", |_root| {
            let bytes = b"a-stub-payload-for-the-unit-test".to_vec();
            let sha = sha256_hex(&bytes);
            let path = publish(&bytes, &sha).expect("publish");
            assert!(path.exists());
            assert!(path.to_string_lossy().contains(&format!("sha256-{sha}")));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);

            let loaded = load(&sha).expect("load re-hashes and returns");
            assert_eq!(loaded, path);
        });
    }

    #[test]
    fn publish_refuses_wrong_pin() {
        with_cache("wrongpin", |root| {
            let bytes = b"some-bytes".to_vec();
            let wrong = "0".repeat(64);
            assert!(publish(&bytes, &wrong).is_err(), "must refuse a mismatched pin");
            // Nothing published.
            let rt = root.join("rt");
            let empty = !rt.exists()
                || std::fs::read_dir(&rt).map(|mut it| it.next().is_none()).unwrap_or(true);
            assert!(empty, "nothing may be published on a pin mismatch");
        });
    }

    #[test]
    fn load_rehashes_and_evicts_on_corruption() {
        with_cache("corrupt", |_root| {
            let bytes = b"trustworthy-bytes".to_vec();
            let sha = sha256_hex(&bytes);
            let path = publish(&bytes, &sha).expect("publish");

            // Bit-flip the cached file on disk.
            let mut corrupt = std::fs::read(&path).unwrap();
            corrupt[0] ^= 0xFF;
            std::fs::write(&path, &corrupt).unwrap();

            // load() must re-hash, detect the mismatch, evict, and return None.
            assert!(load(&sha).is_none(), "a bit-flipped entry must not be trusted by path");
            assert!(!path.exists(), "the corrupt slot must be evicted");
        });
    }

    #[test]
    fn load_miss_is_none() {
        with_cache("miss", |_root| {
            assert!(load(&"f".repeat(64)).is_none());
        });
    }
}
