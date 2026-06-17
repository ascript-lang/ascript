//! RT §5.2 — fail-closed stub fetch.
//!
//! Fetch `{base}/v{version}/rt-manifest.json` (+ `.sig`), VERIFY (the four §5.1
//! checks), fetch the pinned blob, byte-pin it (sha256 + size), then publish into the
//! content-addressed cache ([`crate::rtstub::cache`]).
//!
//! **Fail-closed vs fail-through (RT §5.4):** an *integrity* failure (bad signature,
//! version mismatch, checksum/size mismatch) is [`FetchError::Integrity`] and ABORTS the
//! build — it can never be "recovered" by falling to a weaker rung. A pure *availability*
//! failure (offline, 404, `--no-fetch`, or no `rt-fetch` feature) is
//! [`FetchError::Unavailable`] and the resolution ladder falls through.
//!
//! `base` defaults to the GitHub-releases download URL; `ASCRIPT_RT_BASE_URL` overrides
//! it (mirrors/air-gapped registries) and `file://` is supported for hermetic tests —
//! **the override moves the BYTES, never the TRUST**: the same compiled-in (or test-
//! injected) ed25519 key verifies the same signed manifest. `--no-fetch` /
//! `ASCRIPT_RT_NO_FETCH=1` skips the network entirely.

#[cfg(feature = "rt-fetch")]
use crate::rtstub::cache;
#[cfg(feature = "rt-fetch")]
use crate::rtstub::manifest;
use crate::rtstub::tiers::Tier;
use std::path::PathBuf;
#[cfg(feature = "rt-fetch")]
use std::sync::atomic::{AtomicU64, Ordering};

/// The default release-download base (GitHub releases). `ASCRIPT_RT_BASE_URL` or
/// `FetchOpts.base_url` override it.
#[cfg(feature = "rt-fetch")]
const DEFAULT_BASE_URL: &str =
    "https://github.com/ascript-lang/ascript/releases/download";

/// A global probe counter incremented on each ACTUAL fetch attempt (a manifest read /
/// network call). Tests assert it does NOT advance under `--no-fetch`, proving the
/// network is never touched. Production code never reads it.
#[cfg(feature = "rt-fetch")]
static FETCH_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// The number of fetch attempts made so far (test probe seam; see [`FETCH_ATTEMPTS`]).
#[cfg(feature = "rt-fetch")]
#[doc(hidden)]
pub fn fetch_attempts() -> u64 {
    FETCH_ATTEMPTS.load(Ordering::SeqCst)
}

/// Options controlling a stub fetch.
#[derive(Default)]
pub struct FetchOpts {
    /// Override the release-download base (else `ASCRIPT_RT_BASE_URL`, else the default
    /// GitHub releases URL). Supports `file://` for hermetic mirrors/tests. Moves the
    /// bytes, NOT the trust.
    pub base_url: Option<String>,
    /// `--no-fetch` / `ASCRIPT_RT_NO_FETCH`: skip the network entirely (an availability
    /// fall-through, never an integrity bypass).
    pub no_fetch: bool,
    /// TEST SEAM: inject a verifying pubkey (the test keypair). `None` → the compiled-in
    /// production key. This is NOT an env var — there is no insecure runtime knob.
    #[doc(hidden)]
    pub pubkey: Option<[u8; 32]>,
}

/// A fetch outcome failure, distinguishing the two RT §5.4 categories.
#[derive(Debug)]
pub enum FetchError {
    /// An integrity failure — bad signature, version mismatch, checksum/size mismatch.
    /// FATAL: the build aborts; never falls through to a weaker rung.
    Integrity(String),
    /// A pure availability failure — offline, 404, `--no-fetch`, or no `rt-fetch`
    /// feature. The resolution ladder falls through to the dev fallbacks.
    Unavailable(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Integrity(m) => write!(f, "stub integrity failure: {m}"),
            FetchError::Unavailable(m) => write!(f, "stub unavailable: {m}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Resolve the effective release base: explicit `base_url` override, else
/// `$ASCRIPT_RT_BASE_URL`, else the default GitHub-releases URL. Trailing slashes are
/// trimmed so URL joins are clean.
#[cfg(feature = "rt-fetch")]
fn resolve_base(opts: &FetchOpts) -> String {
    let raw = opts
        .base_url
        .clone()
        .or_else(|| std::env::var("ASCRIPT_RT_BASE_URL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Fetch + verify + publish the stub for `(target, tier)`. See module docs for the
/// fail-closed contract.
///
/// Resolution: honor `--no-fetch`/`ASCRIPT_RT_NO_FETCH` (availability fall-through) →
/// fetch manifest + sig → verify the four §5.1 checks → entry lookup → fetch blob →
/// byte-pin → [`cache::publish`]. Any integrity failure refuses BEFORE anything is
/// published.
pub async fn fetch_stub(
    target: &str,
    tier: Tier,
    opts: &FetchOpts,
) -> Result<PathBuf, FetchError> {
    // --no-fetch / ASCRIPT_RT_NO_FETCH: skip the network ENTIRELY (no attempt counted).
    let no_fetch = opts.no_fetch
        || std::env::var("ASCRIPT_RT_NO_FETCH").map(|v| v == "1").unwrap_or(false);
    if no_fetch {
        return Err(FetchError::Unavailable(
            "--no-fetch is set: not contacting the release host".to_string(),
        ));
    }

    fetch_impl(target, tier, opts).await
}

#[cfg(feature = "rt-fetch")]
async fn fetch_impl(
    target: &str,
    tier: Tier,
    opts: &FetchOpts,
) -> Result<PathBuf, FetchError> {
    let version = env!("CARGO_PKG_VERSION");
    let base = resolve_base(opts);
    let vbase = format!("{base}/v{version}");

    // 1. Fetch the manifest + detached signature (availability failures fall through).
    let manifest_bytes = get_bytes(&format!("{vbase}/rt-manifest.json"))
        .await
        .map_err(FetchError::Unavailable)?;
    let sig_bytes = get_bytes(&format!("{vbase}/rt-manifest.json.sig"))
        .await
        .map_err(FetchError::Unavailable)?;

    // 2. Verify (checks 1+2: signature over exact bytes, version lock). INTEGRITY-fatal.
    let pubkey = opts.pubkey.unwrap_or(manifest::PRODUCTION_PUBKEY);
    let manifest = manifest::verify_manifest(&manifest_bytes, &sig_bytes, &pubkey)
        .map_err(FetchError::Integrity)?;

    // 3. Entry lookup by (target, tier). INTEGRITY-fatal (a missing entry is refusal,
    //    not a fall-through — the signed manifest does not vouch for this stub).
    let entry = manifest.entry_for(target, tier.name()).ok_or_else(|| {
        FetchError::Integrity(format!(
            "the signed manifest has no entry for target '{target}' tier '{}'",
            tier.name()
        ))
    })?;

    // 4. Fetch the pinned blob (availability), then byte-pin it BEFORE publishing.
    let blob = get_bytes(&format!("{vbase}/{}", entry.filename))
        .await
        .map_err(FetchError::Unavailable)?;

    if blob.len() as u64 != entry.size {
        return Err(FetchError::Integrity(format!(
            "stub size mismatch: manifest pins {} bytes but the blob is {} bytes",
            entry.size,
            blob.len()
        )));
    }
    let actual_sha = cache::sha256_hex(&blob);
    if actual_sha != entry.sha256 {
        return Err(FetchError::Integrity(format!(
            "stub sha256 mismatch: manifest pins {} but the blob hashes to {actual_sha}",
            entry.sha256
        )));
    }

    // Byte pin holds → publish atomically into the content-addressed cache.
    cache::publish(&blob, &entry.sha256).map_err(FetchError::Integrity)
}

/// The no-feature stub: with `rt-fetch` disabled there is no signature verifier, so the
/// network rung is simply UNAVAILABLE — an availability fall-through to the dev
/// fallbacks (NOT an integrity bypass; nothing is fetched or trusted).
#[cfg(not(feature = "rt-fetch"))]
async fn fetch_impl(
    _target: &str,
    _tier: Tier,
    _opts: &FetchOpts,
) -> Result<PathBuf, FetchError> {
    Err(FetchError::Unavailable(
        "this build has no 'rt-fetch' feature — the network rung is unavailable; \
         use --stub or a sibling ascript-rt"
            .to_string(),
    ))
}

/// Fetch bytes from `url`, supporting `file://` for hermetic tests and `reqwest` for
/// real URLs. Counts ONE fetch attempt (the probe seam) per call. Returns an
/// availability-style error string on any I/O / status failure.
#[cfg(feature = "rt-fetch")]
async fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    FETCH_ATTEMPTS.fetch_add(1, Ordering::SeqCst);

    if let Some(local) = url.strip_prefix("file://") {
        return std::fs::read(local)
            .map_err(|e| format!("cannot read {local}: {e}"));
    }

    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("failed to fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("fetch {url} failed with status {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("failed to read {url}: {e}"))?;
    Ok(bytes.to_vec())
}
