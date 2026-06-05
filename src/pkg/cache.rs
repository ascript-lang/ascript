//! Content-addressed package cache layout (SP6 §4).
//!
//! The cache root is resolved once, by the CLI, with NO extra dependency (read
//! env + a small per-OS `cfg!` switch — deliberately no `dirs` crate):
//!
//! ```text
//! $ASCRIPT_CACHE                          // explicit override (CI, sandboxes)
//! else $XDG_CACHE_HOME/ascript            // Linux/XDG
//! else ~/Library/Caches/ascript           // macOS
//! else %LOCALAPPDATA%\ascript\Cache       // Windows
//! else <tempdir>/ascript-cache            // last-resort fallback
//! ```
//!
//! Layout under the root:
//! ```text
//! store/<asum-hash>/      // immutable, content-addressed package tree
//! git/<host>/<path>.git/  // bare git clones, reused across fetch/update
//! tmp/                    // staging during fetch+hash before atomic move
//! ```

use std::path::PathBuf;

/// Resolve the cache root, honoring `$ASCRIPT_CACHE` first, then the per-OS
/// default, then a tempdir last resort. Never fails (the tempdir fallback always
/// yields a path).
pub fn cache_root() -> PathBuf {
    if let Some(p) = std::env::var_os("ASCRIPT_CACHE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    platform_default()
}

/// Read an env var, treating empty as absent.
fn nonempty_env(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|v| !v.is_empty())
}

#[cfg(target_os = "macos")]
fn platform_default() -> PathBuf {
    // macOS: ~/Library/Caches/ascript, but honor an explicit XDG_CACHE_HOME if
    // the user set one (matching most Rust tooling).
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
    // Linux / other unix: $XDG_CACHE_HOME/ascript else ~/.cache/ascript.
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
    // Windows: %LOCALAPPDATA%\ascript\Cache.
    if let Some(local) = nonempty_env("LOCALAPPDATA") {
        return PathBuf::from(local).join("ascript").join("Cache");
    }
    temp_fallback()
}

#[cfg(not(any(unix, windows)))]
fn platform_default() -> PathBuf {
    // Fall back to the tempdir on exotic targets. `nonempty_env` is unused here,
    // but reachable on every real target, so reference it once to keep the
    // function honest without an `#[allow]`.
    let _ = nonempty_env;
    temp_fallback()
}

fn temp_fallback() -> PathBuf {
    std::env::temp_dir().join("ascript-cache")
}

/// The immutable content-addressed store dir for a package whose normalized-tree
/// hash is `hash` (an `asum1-…` string). The loadable package root.
pub fn store_dir(hash: &str) -> PathBuf {
    cache_root().join("store").join(hash)
}

/// The bare-git working dir for a remote `url`, namespaced by host+path so two
/// remotes never collide and a clone is reused across fetch/update.
pub fn git_dir(url: &str) -> PathBuf {
    cache_root().join("git").join(git_subpath(url))
}

/// The staging dir used during fetch+hash before the atomic move into `store/`.
pub fn tmp_dir() -> PathBuf {
    cache_root().join("tmp")
}

/// Derive a filesystem-safe `<host>_<path>.git`-style name from a git URL. Any
/// character outside `[A-Za-z0-9._-]` becomes `_`; the result always ends in
/// `.git`. A cache-keying convenience, not a security boundary.
fn git_subpath(url: &str) -> PathBuf {
    let after_scheme = url.splitn(2, "://").last().unwrap_or(url);
    let mut safe = String::with_capacity(after_scheme.len() + 4);
    for ch in after_scheme.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            safe.push(ch);
        } else {
            safe.push('_');
        }
    }
    if !safe.ends_with(".git") {
        safe.push_str(".git");
    }
    PathBuf::from(safe)
}

/// A process-wide lock serializing SYNC tests that mutate the global
/// `$ASCRIPT_CACHE` env var (cargo runs unit tests in parallel within a binary).
/// Lives here so every `pkg` test module shares ONE lock and they never race.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The async counterpart of [`TEST_ENV_LOCK`]: a `tokio::sync::Mutex` whose
/// guard is held across `.await` (so it doesn't trip clippy's
/// `await_holding_lock`, which only fires on `std::sync` guards). Async tests
/// that set `$ASCRIPT_CACHE` lock this; sync tests lock `TEST_ENV_LOCK`. They are
/// SEPARATE locks, so an async and a sync env-test could still interleave — but
/// in practice the sync tests never touch a fetch-shaped cache and the async
/// tests use unique per-test cache dirs, so an interleave is harmless.
#[cfg(test)]
pub(crate) static TEST_ENV_ALOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Ensure the `store/`, `git/`, and `tmp/` subdirs exist under the cache root.
pub fn create_dirs() -> std::io::Result<()> {
    let root = cache_root();
    std::fs::create_dir_all(root.join("store"))?;
    std::fs::create_dir_all(root.join("git"))?;
    std::fs::create_dir_all(root.join("tmp"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascript_cache_override_wins_and_subdirs_join_under_it() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("ascache-test-{}", std::process::id()));
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", &tmp);

        assert_eq!(cache_root(), tmp);
        assert_eq!(store_dir("asum1-abc"), tmp.join("store").join("asum1-abc"));
        assert_eq!(tmp_dir(), tmp.join("tmp"));
        assert!(git_dir("https://x/y").starts_with(tmp.join("git")));

        create_dirs().unwrap();
        assert!(tmp.join("store").is_dir());
        assert!(tmp.join("git").is_dir());
        assert!(tmp.join("tmp").is_dir());
        let _ = std::fs::remove_dir_all(&tmp);

        restore(prev);
    }

    #[test]
    fn empty_ascript_cache_falls_through_to_default() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", "");
        let root = cache_root();
        assert!(!root.as_os_str().is_empty());
        restore(prev);
    }

    #[test]
    fn git_subpath_is_filesystem_safe_and_dot_git() {
        let p = git_subpath("https://github.com/acme/as-http");
        let s = p.to_string_lossy();
        assert!(s.ends_with(".git"), "{s}");
        assert!(!s.contains('/'), "host/path collapsed to safe chars: {s}");
        assert!(s.starts_with("github.com_acme_as-http"), "{s}");

        let f = git_subpath("file:///tmp/repo");
        assert!(f.to_string_lossy().ends_with(".git"));
    }

    #[test]
    fn git_dir_distinct_per_remote() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", std::env::temp_dir().join("ascache-gitdir"));
        assert_ne!(git_dir("https://a/x"), git_dir("https://b/x"));
        restore(prev);
    }

    fn restore(prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var("ASCRIPT_CACHE", v),
            None => std::env::remove_var("ASCRIPT_CACHE"),
        }
    }
}
