//! RT §4.5 — `--exact`: a local `cargo build` of `ascript-rt` with EXACTLY the
//! features the program needs (no tier slack).
//!
//! The core of this module is **PURE** — `exact_build_plan` does no I/O and is
//! unit-tested directly. The impure parts (cargo detection, invocation, signing,
//! caching) are behind a seam struct so tests can inject fixtures hermetically.
//!
//! Compiled only in the TOOLCHAIN build (`#[cfg(not(ascript_rt))]`). Must build
//! under `--no-default-features`.
//!
//! # Invariants
//! - `--exact --target *-apple-darwin` on a non-macOS host is rejected before
//!   invoking cargo (the `apple-codesign` dep is macOS-host-gated).
//! - Detection failures produce SPECIFIC Tier-1 errors naming what to fix.
//! - macOS host: the built stub is ad-hoc signed BEFORE being published to the
//!   content-addressed cache (the sign-before-append rule §6.2).
//! - A second `--exact` with the SAME (version, target, sorted-features) reuses
//!   the cached stub without re-invoking cargo (the `exact-index` sidecar).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ── Pure core ──────────────────────────────────────────────────────────────────

/// Build the `cargo` argv and env for an exact stub build.
///
/// **PURE — no I/O.** Returns `(argv, env)` where:
/// - `argv` is the arguments to pass to `cargo` (starting with `"build"`).
/// - `env` is the `(key, value)` pairs to set in the subprocess environment.
///
/// The features argument is the sorted, deduplicated union of `required` plus
/// `"bundle-zstd"` (always present — the stub needs the decompressor) and
/// `"shared"` (always present — core, as `scripts/build-rt.sh rt-core` uses).
///
/// When `target` is `Some` the returned argv includes `--target <triple>`.
pub fn exact_build_plan(
    required: &BTreeSet<&str>,
    target: Option<&str>,
) -> (Vec<String>, Vec<(String, String)>) {
    // Always include the mandatory features.
    let mut features: BTreeSet<&str> = required.iter().copied().collect();
    features.insert("bundle-zstd");
    features.insert("shared");

    let features_str: String = features
        .iter()
        .copied()
        .collect::<Vec<&str>>()
        .join(",");

    let mut argv: Vec<String> = vec![
        "build".to_string(),
        "--release".to_string(),
        "--bin".to_string(),
        "ascript-rt".to_string(),
        "--no-default-features".to_string(),
        "--features".to_string(),
        features_str,
    ];

    if let Some(t) = target {
        argv.push("--target".to_string());
        argv.push(t.to_string());
    }

    let env: Vec<(String, String)> = vec![
        ("ASCRIPT_RT".to_string(), "1".to_string()),
        ("ASCRIPT_RT_TIER".to_string(), "custom".to_string()),
    ];

    (argv, env)
}

// ── Detection seam ─────────────────────────────────────────────────────────────

/// Injectable context for hermetic unit-testing of the detection logic.
/// Production code uses [`DetectContext::real()`].
#[doc(hidden)]
#[derive(Debug)]
pub struct DetectContext {
    /// Whether `cargo` is available on `PATH`.
    pub cargo_available: bool,
    /// The `$ASCRIPT_SRC` path override, if any.
    pub ascript_src: Option<PathBuf>,
    /// Version string to compare against `env!("CARGO_PKG_VERSION")`.
    /// When `None` the context reads `Cargo.toml` from the given path.
    pub version_override: Option<String>,
}

impl DetectContext {
    /// Build a context from the real environment (reads `$ASCRIPT_SRC` + the
    /// actual filesystem).
    pub fn real() -> Self {
        let cargo_available = which_cargo();
        let ascript_src = std::env::var_os("ASCRIPT_SRC")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        DetectContext {
            cargo_available,
            ascript_src,
            version_override: None,
        }
    }
}

/// Check whether `cargo` can be found on PATH.
fn which_cargo() -> bool {
    std::process::Command::new("cargo")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Parse the `[package] version` field from a `Cargo.toml` file.
fn read_cargo_toml_version(path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Simple scan — no toml dep needed for a single field.
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                let rest = rest.trim_matches('"');
                if !rest.is_empty() && !rest.contains('[') {
                    return Ok(rest.to_string());
                }
            }
        }
    }
    Err(format!(
        "no [package] version found in {}",
        path.display()
    ))
}

/// Detection result from [`detect`].
#[derive(Debug)]
pub struct Detected {
    /// The validated `$ASCRIPT_SRC` path.
    pub src_path: PathBuf,
}

/// Validate the build environment: cargo on PATH, `$ASCRIPT_SRC` set with a
/// matching package version. Returns a specific Tier-1 error for each failure.
///
/// Accepts a [`DetectContext`] for hermetic testing.
pub fn detect(ctx: &DetectContext) -> Result<Detected, String> {
    if !ctx.cargo_available {
        return Err(
            "--exact requires cargo: install Rust (https://rustup.rs/) so `cargo` is on \
             your PATH, then retry"
                .to_string(),
        );
    }

    let src_path = match &ctx.ascript_src {
        Some(p) => p.clone(),
        None => {
            return Err(
                "--exact requires $ASCRIPT_SRC to be set to the root of an AScript source \
                 checkout (the directory containing Cargo.toml)"
                    .to_string(),
            );
        }
    };

    // Version check: the checkout must match this toolchain's version.
    let this_version = env!("CARGO_PKG_VERSION");
    let manifest_version = match &ctx.version_override {
        Some(v) => v.clone(),
        None => {
            let manifest = src_path.join("Cargo.toml");
            read_cargo_toml_version(&manifest)
                .map_err(|e| format!("$ASCRIPT_SRC version check failed: {e}"))?
        }
    };
    if manifest_version != this_version {
        return Err(format!(
            "$ASCRIPT_SRC version mismatch: the source checkout at {} is version \
             '{manifest_version}' but this toolchain is version '{this_version}' — \
             check out the matching tag or set $ASCRIPT_SRC to a checkout of v{this_version}",
            src_path.display()
        ));
    }

    Ok(Detected { src_path })
}

// ── Darwin-cross rejection ──────────────────────────────────────────────────────

/// Returns `true` when the target is a macOS triple (regardless of host).
fn is_darwin_target(target: Option<&str>) -> bool {
    match target {
        None => false,
        Some(t) => t.ends_with("-apple-darwin"),
    }
}

/// Returns `true` when the current host is macOS.
fn is_macos_host() -> bool {
    cfg!(target_os = "macos")
}

/// Reject `--exact --target *-apple-darwin` on a non-macOS host (§4.5: the
/// `apple-codesign` dependency is macOS-host-gated; prebuilt signed darwin stubs
/// are the cross answer).
pub fn check_darwin_cross(target: Option<&str>) -> Result<(), String> {
    if is_darwin_target(target) && !is_macos_host() {
        return Err(format!(
            "--exact --target '{}' is not supported on a non-macOS host: \
             building a Darwin stub requires the apple-codesign toolchain which is \
             macOS-only. Use a prebuilt stub with --stub, or fetch one with the \
             network rung (omit --exact).",
            target.unwrap_or("*-apple-darwin")
        ));
    }
    Ok(())
}

// ── Exact-index sidecar ─────────────────────────────────────────────────────────

/// A key into the exact-index sidecar: a deterministic fingerprint of the
/// (version, target, sorted feature set) triple that uniquely identifies a build
/// configuration.
fn index_key(version: &str, target: Option<&str>, features: &BTreeSet<&str>) -> String {
    let t = target.unwrap_or("host");
    let feat: Vec<&str> = features.iter().copied().collect();
    format!("{version}|{t}|{}", feat.join(","))
}

/// The path to the exact-index sidecar file under `$ASCRIPT_CACHE/rt/`.
fn exact_index_path() -> PathBuf {
    super::cache::cache_root()
        .join("rt")
        .join("exact-index.json")
}

/// Read the exact-index sidecar and return the stub sha256 for `key`, if any.
fn index_lookup(key: &str) -> Option<String> {
    let path = exact_index_path();
    let content = std::fs::read_to_string(&path).ok()?;
    // Simple linear scan: each line is `<key>=<sha256>`.
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Append `key=sha256` to the exact-index sidecar (creates or appends).
fn index_record(key: &str, sha256: &str) -> Result<(), String> {
    use std::io::Write;
    let path = exact_index_path();
    // Ensure the parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create rt cache dir: {e}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open exact-index: {e}"))?;
    writeln!(file, "{key}={sha256}")
        .map_err(|e| format!("cannot write exact-index: {e}"))?;
    Ok(())
}

// ── Output path ───────────────────────────────────────────────────────────────

/// The path to the built `ascript-rt` binary after a successful cargo build.
fn built_stub_path(src: &Path, target: Option<&str>) -> PathBuf {
    let filename = if cfg!(windows) { "ascript-rt.exe" } else { "ascript-rt" };
    match target {
        Some(t) => src.join("target").join(t).join("release").join(filename),
        None => src.join("target").join("release").join(filename),
    }
}

// ── Main entry point ──────────────────────────────────────────────────────────

/// The result of a successful exact build or cache hit.
pub struct ExactResolved {
    /// Path to the (clean) stub binary bytes — content-addressed slot in the cache.
    pub bytes_path: PathBuf,
    /// The sha256 of the stub bytes (the content-addressed cache key).
    pub sha256: String,
    /// Whether the result came from the cache (no cargo invoked).
    pub from_cache: bool,
}

/// Perform an `--exact` build: detect the environment, check for a cache hit via
/// the exact-index, invoke cargo on a miss, sign (macOS), publish to the
/// content-addressed cache, and record in the exact-index.
///
/// `required` must already include the transitive feature closure; `target` is the
/// `--target` triple (None = host). Accepts a [`DetectContext`] for testing.
pub fn build_exact(
    required: &BTreeSet<&str>,
    target: Option<&str>,
    ctx: &DetectContext,
) -> Result<ExactResolved, String> {
    // §4.5: darwin target on a non-mac host → reject immediately.
    check_darwin_cross(target)?;

    // Validate the environment (cargo + $ASCRIPT_SRC + version).
    let detected = detect(ctx)?;

    // The full feature set for the plan (required + mandatory).
    let mut all_features: BTreeSet<&str> = required.iter().copied().collect();
    all_features.insert("bundle-zstd");
    all_features.insert("shared");

    let version = env!("CARGO_PKG_VERSION");
    let key = index_key(version, target, &all_features);

    // Cache hit: exact-index has a sha256 → try the content-addressed cache.
    if let Some(cached_sha) = index_lookup(&key) {
        if let Some(path) = super::cache::load(&cached_sha) {
            return Ok(ExactResolved {
                bytes_path: path,
                sha256: cached_sha,
                from_cache: true,
            });
        }
        // Cache slot evicted (corrupted / deleted) — fall through to a rebuild.
    }

    // Build the stub via cargo.
    let (argv, env_vars) = exact_build_plan(required, target);

    let mut cmd = std::process::Command::new("cargo");
    for arg in &argv {
        cmd.arg(arg);
    }
    for (k, v) in &env_vars {
        cmd.env(k, v);
    }
    cmd.current_dir(&detected.src_path);
    // Inherit stderr so cargo's progress / error output surfaces verbatim.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd
        .status()
        .map_err(|e| format!("cargo invocation failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build of ascript-rt failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }

    // Locate the built binary.
    let built = built_stub_path(&detected.src_path, target);
    if !built.exists() {
        return Err(format!(
            "cargo build succeeded but the output binary was not found at {}",
            built.display()
        ));
    }

    // macOS host + darwin target → ad-hoc sign BEFORE publishing to the cache.
    // §6.2: sign before append — the signature covers [0, stub_len).
    #[cfg(target_os = "macos")]
    if target.map(|t| t.ends_with("-apple-darwin")).unwrap_or(true) {
        crate::bundle::adhoc_sign_macos(&built)?;
    }

    // Read, hash, and publish to the content-addressed cache.
    let bytes = std::fs::read(&built)
        .map_err(|e| format!("cannot read built stub at {}: {e}", built.display()))?;
    let sha256 = super::cache::sha256_hex(&bytes);
    let cache_path = super::cache::publish(&bytes, &sha256)?;

    // Record in the exact-index for future cache hits.
    let _ = index_record(&key, &sha256); // best-effort — a write failure does not abort the build

    Ok(ExactResolved {
        bytes_path: cache_path,
        sha256,
        from_cache: false,
    })
}
