//! RT §4.3–§4.4 — the selection entry the builder calls.
//!
//! Takes the archive's std imports → [`required_features`] → [`select_tier`] (or honors
//! a forced `--tier` with the downgrade check), and returns the chosen [`Tier`] plus the
//! required/unused feature breakdown the build report (§4.6/§9.2) surfaces.
//!
//! Task 7 will extend this into the full stub-resolution ladder (§5.4); for now it
//! resolves only WHICH tier the bundle is logically built against — the actual stub stays
//! `current_exe()` until the ladder lands.
//!
//! Compiled only in the TOOLCHAIN build (`#[cfg(not(ascript_rt))]`). Must build under
//! `--no-default-features`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::std_features::{required_features, STD_MODULE_FEATURES};
use super::tiers::{select_tier, validate_forced_tier, Tier};

/// The outcome of tier selection — the chosen tier, where it came from, and the
/// required/unused feature breakdown for the report.
#[derive(Debug, Clone)]
pub struct Selection {
    /// The chosen stub tier.
    pub tier: Tier,
    /// How the tier was chosen: automatic nearest-superset, or a forced `--tier`.
    pub source: TierSource,
    /// The features the program's imports actually require (closure-expanded), sorted.
    pub required: Vec<String>,
    /// The tier's full feature set, sorted (what the stub ships).
    pub stub: Vec<String>,
    /// `stub \ required` — the features the chosen tier carries that the program does
    /// not need. The user's lever to see what `--exact` (Task 8) would save.
    pub unused: Vec<String>,
}

/// Where a tier choice came from (mirrors the `tier_source` field of §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierSource {
    /// Automatic nearest-superset selection (§4.4 default).
    Selected,
    /// Forced via the `--tier` CLI flag.
    Tier,
}

impl TierSource {
    /// The schema-1 `tier_source` string (§9.2).
    pub fn as_str(self) -> &'static str {
        match self {
            TierSource::Selected => "selected",
            TierSource::Tier => "--tier",
        }
    }
}

/// Map each std import specifier to the feature it directly demands (skipping core
/// modules), for attribution in the downgrade error (§4.4). Closure deps are NOT
/// attributed here — only the directly-named feature, which is what a user can act on.
fn demanding_modules(std_imports: &BTreeSet<String>) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for spec in std_imports {
        if let Some((_, Some(feat))) = STD_MODULE_FEATURES.iter().find(|(m, _)| *m == spec.as_str())
        {
            out.push((spec.clone(), *feat));
        }
    }
    out
}

/// Select the stub tier for a set of std imports.
///
/// - `forced` is `Some` when the user passed `--tier`; the downgrade check (§4.4) then
///   errors (listing missing features + demanding modules) if the forced tier does not
///   cover the program's requirements.
/// - `forced == None` ⇒ automatic nearest-superset selection.
///
/// Returns `Err` for an unknown std specifier (it would mean STD_MODULES drift) or for a
/// forced tier that is insufficient.
pub fn select(
    std_imports: &BTreeSet<String>,
    forced: Option<Tier>,
) -> Result<Selection, String> {
    let required: BTreeSet<&str> = required_features(std_imports)?;

    let (tier, source) = match forced {
        Some(t) => {
            validate_forced_tier(t, &required, &demanding_modules(std_imports))?;
            (t, TierSource::Tier)
        }
        None => (select_tier(&required), TierSource::Selected),
    };

    let stub_set = tier.feature_set();
    let required_vec: Vec<String> = required.iter().map(|s| s.to_string()).collect();
    let stub_vec: Vec<String> = {
        let mut v: Vec<String> = stub_set.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    let unused: Vec<String> = {
        let mut v: Vec<String> = stub_set
            .iter()
            .filter(|f| !required.contains(*f))
            .map(|s| s.to_string())
            .collect();
        v.sort();
        v
    };

    Ok(Selection {
        tier,
        source,
        required: required_vec,
        stub: stub_vec,
        unused,
    })
}

// ───────────────────────────────────────────────────────────────────────────────────────
// RT §5.4 / §6 — the stub resolution ladder.
//
// `resolve_stub` walks the five rungs in order, with the integrity-vs-availability split:
// an INTEGRITY failure (a tampered fetched stub, a tier-insufficient `--stub`) ABORTS the
// build; an AVAILABILITY failure (offline, no sibling, a cross target) falls through to the
// next rung. Cross targets (a `--target` ≠ host) stop after rung 3 — no local binary can
// serve a foreign triple.
// ───────────────────────────────────────────────────────────────────────────────────────

/// RT §3.3 — the published cross-build target set (8 triples). A `--target` outside this set
/// is rejected with an error listing the supported triples (the ladder is never even walked).
pub const SUPPORTED_TARGETS: &[&str] = &[
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The Rust target triple this toolchain binary was built for (the host). A `--target`
/// equal to this is treated exactly like omitting `--target` (host build).
pub fn host_target() -> &'static str {
    env!("TARGET")
}

/// The resolved stub the payload will be appended to (RT §5.4). `bytes_path` is a CLEAN
/// runtime (any pre-existing overlay already stripped by `build_native`); `origin` is the
/// short ladder-rung tag for the build report; `sha256` identifies the clean stub bytes;
/// `features` is the stub's feature set when known (rung-1/4 `--rt-info` probe, rung-2/3
/// manifest), or `None` when unverifiable (a cross-target `--stub` — the user asserts
/// compatibility, reported as `features: unverified`).
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Path to the (clean) stub binary bytes on disk.
    pub bytes_path: PathBuf,
    /// Short ladder-rung origin tag (`"--stub"`/`"cache"`/`"fetch"`/`"sibling"`/`"current_exe"`).
    pub origin: &'static str,
    /// Lowercase-hex sha256 of the clean stub bytes.
    pub sha256: String,
    /// The stub's feature set when verifiable; `None` for an unverifiable cross `--stub`.
    pub features: Option<Vec<String>>,
}

/// Inputs to [`resolve_stub`] — the subset of `NativeBuildOpts` + the required features the
/// ladder needs. Kept dependency-free (no `NativeBuildOpts` import) so `select` stays a leaf.
#[derive(Debug, Clone)]
pub struct ResolveOpts {
    /// `--target` triple (already validated against [`SUPPORTED_TARGETS`]). `None` ⇒ host.
    pub target: Option<String>,
    /// The chosen tier (from [`select`]).
    pub tier: Tier,
    /// `--stub <path>` — explicit local stub (rung 1).
    pub stub: Option<PathBuf>,
    /// `--no-fetch` — skip the network rung (3).
    pub no_fetch: bool,
    /// The features the program's imports require (for the §4.3 tier-validation check).
    pub required_features: Vec<String>,
    /// The std imports that demand each missing feature, for the failure message.
    pub demanding: Vec<(String, String)>,
}

/// Whether the requested target is the host (`None` or `Some(host)`).
fn is_host_target(target: &Option<String>) -> bool {
    match target {
        None => true,
        Some(t) => t == host_target(),
    }
}

/// The effective target triple string (host when `None`).
fn effective_target(target: &Option<String>) -> &str {
    match target {
        Some(t) => t.as_str(),
        None => host_target(),
    }
}

/// Run `<stub> --rt-info` and parse the `features` array from its one-line JSON. Returns the
/// feature set, or `Err` if the stub is not executable here / fails / prints no parsable JSON.
/// Used by rungs 1 and 4 (host-executable stubs only).
fn probe_rt_info_features(stub: &Path) -> Result<Vec<String>, String> {
    let out = std::process::Command::new(stub)
        .arg("--rt-info")
        .output()
        .map_err(|e| format!("could not run '{} --rt-info': {e}", stub.display()))?;
    if !out.status.success() {
        return Err(format!(
            "'{} --rt-info' failed (exit {:?})",
            stub.display(),
            out.status.code()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_rt_info_features(&text)
        .ok_or_else(|| format!("'{} --rt-info' did not emit a parsable features array", stub.display()))
}

/// Extract the `"features":[…]` array from an `--rt-info` JSON line (no serde dep — the rt
/// stub emits a flat one-line object). Returns `None` if the array is absent/malformed.
fn parse_rt_info_features(json: &str) -> Option<Vec<String>> {
    let needle = "\"features\":[";
    let start = json.find(needle)? + needle.len();
    let end = json[start..].find(']')? + start;
    let arr = &json[start..end];
    let mut out = Vec::new();
    for raw in arr.split(',') {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let s = s.strip_prefix('"').unwrap_or(s);
        let s = s.strip_suffix('"').unwrap_or(s);
        out.push(s.to_string());
    }
    Some(out)
}

/// RT §4.3 — structural tier validation: the chosen stub's `features` must be a SUPERSET of
/// the program's `required_features`. On a shortfall, a fail-closed error lists the missing
/// features and the modules that demand them. `None` features (an unverifiable cross `--stub`)
/// SKIPS the check — the user asserts compatibility (reported as `features: unverified`).
fn validate_stub_features(
    stub_features: &Option<Vec<String>>,
    required: &[String],
    demanding: &[(String, String)],
) -> Result<(), String> {
    let have = match stub_features {
        Some(f) => f,
        None => return Ok(()), // unverifiable cross-target stub — user asserts compatibility
    };
    let have_set: BTreeSet<&str> = have.iter().map(|s| s.as_str()).collect();
    let missing: Vec<&str> = required
        .iter()
        .map(|s| s.as_str())
        .filter(|f| !have_set.contains(f))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut attributions: Vec<String> = Vec::new();
    for &feat in &missing {
        let mut mods: Vec<&str> = demanding
            .iter()
            .filter(|(_, f)| f == feat)
            .map(|(m, _)| m.as_str())
            .collect();
        mods.sort_unstable();
        mods.dedup();
        if mods.is_empty() {
            attributions.push(format!("'{feat}'"));
        } else {
            attributions.push(format!("'{feat}' (required by {})", mods.join(", ")));
        }
    }
    Err(format!(
        "the chosen stub is missing {} the program requires — {}. The stub does not provide \
         every imported module's feature; pick a stub built with a sufficient tier.",
        if missing.len() == 1 { "a feature" } else { "features" },
        attributions.join("; "),
    ))
}

/// Lowercase-hex sha256 of `bytes` (stub identity for the report).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Read a stub binary, strip any pre-existing bundle overlay (exactly as `build_native` does
/// for `current_exe()`), write the CLEAN bytes to a temp file under the cache tmp dir, and
/// return its path + sha256. The temp file is what gets appended to (so the resolver never
/// hands back a path that still carries a foreign footer). Returns `(clean_path, sha256)`.
fn materialize_clean_stub(src: &Path, tag: &str) -> Result<(PathBuf, String), String> {
    let raw = std::fs::read(src)
        .map_err(|e| format!("cannot read stub {}: {e}", src.display()))?;
    let clean: &[u8] = match crate::bundle::read_bundle_footer(&raw) {
        Some((offset, _len)) => &raw[..offset], // strip a pre-existing overlay
        None => &raw,
    };
    let sha = sha256_hex(clean);
    if raw.len() == clean.len() {
        // No overlay to strip — the source file is already a clean stub; use it in place.
        return Ok((src.to_path_buf(), sha));
    }
    // An overlay was stripped — write the clean prefix to a temp file under the cache tmp dir.
    let tmp = super::cache::tmp_dir();
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create cache tmp dir: {e}"))?;
    let dest = tmp.join(format!(
        "rt-clean-stub-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&dest, clean).map_err(|e| format!("cannot stage clean stub: {e}"))?;
    Ok((dest, sha))
}

/// RT §5.4 — resolve the stub the payload will be appended to, walking the five rungs. See
/// the module-level section comment for the integrity-vs-availability contract.
///
/// The `fetch` rung is async (it may hit the network); `resolve_stub` is therefore async too.
/// Integrity failures from any rung return `Err` IMMEDIATELY (fail-closed); availability
/// failures are accumulated into `reasons` and the ladder falls through. When every rung is
/// exhausted, the error names each rung's reason and points at `--exact`/`--stub`.
pub async fn resolve_stub(opts: &ResolveOpts) -> Result<Resolved, String> {
    let host = is_host_target(&opts.target);
    let target = effective_target(&opts.target).to_string();
    let mut reasons: Vec<String> = Vec::new();

    // ── Rung 1: --stub <path> ──────────────────────────────────────────────────────────
    if let Some(stub_path) = &opts.stub {
        if !stub_path.exists() {
            return Err(format!(
                "--stub path does not exist: {}",
                stub_path.display()
            ));
        }
        let (clean_path, sha) = materialize_clean_stub(stub_path, "stub")?;
        // Feature-verify via --rt-info when the stub is executable on THIS host. A cross-target
        // --stub (not host-runnable) → features unverified, user asserts compatibility (§4.3).
        let features = if host {
            // The stub could not be probed (not host-executable / not an rt stub)? For a host
            // target this is unusual but not fatal: treat as unverified rather than refusing a
            // legitimate custom stub. (A tier-insufficient probe-able stub is caught below.)
            probe_rt_info_features(&clean_path).ok()
        } else {
            None // cross-target stub: cannot run it here
        };
        // §4.3 — tier validation is FAIL-CLOSED (integrity): a probe-able stub missing a
        // required feature ABORTS the build.
        validate_stub_features(&features, &opts.required_features, &opts.demanding)?;
        return Ok(Resolved {
            bytes_path: clean_path,
            origin: "--stub",
            sha256: sha,
            features,
        });
    }

    // ── Rung 2: cache hit for the manifest-pinned (version, target, tier) digest ────────
    // Without a fetched/known manifest we have no pinned digest to look up by content
    // address, so the cache rung is only reachable after a prior fetch published a blob.
    // We attempt the fetch (rung 3) which itself consults+publishes the cache; an explicit
    // pre-fetch cache probe is not possible without the manifest. (Recorded: the cache is
    // the fetch rung's verify-on-load store; a standalone cache hit needs the digest the
    // manifest provides.) We therefore fold rungs 2+3 into the fetch call, which:
    //   - returns Integrity → ABORT here (fail-closed);
    //   - returns Unavailable → fall through to rungs 4/5.

    // ── Rung 3 (+ 2): fetch (unless --no-fetch) ────────────────────────────────────────
    let fetch_opts = crate::rtstub::fetch::FetchOpts {
        no_fetch: opts.no_fetch,
        ..Default::default()
    };
    match crate::rtstub::fetch::fetch_stub(&target, opts.tier, &fetch_opts).await {
        Ok(path) => {
            // The fetched (or cached) blob is verified by fetch_stub (sha256 + size pin) and
            // re-hashed on publish/load. Its features are known from the signed manifest, but
            // fetch_stub returns only the path; we re-derive the tier's feature set (the
            // manifest's `features` equals the tier's cumulative set by construction).
            let raw = std::fs::read(&path)
                .map_err(|e| format!("cannot read fetched stub {}: {e}", path.display()))?;
            let sha = sha256_hex(&raw);
            let features: Vec<String> =
                opts.tier.features().iter().map(|s| s.to_string()).collect();
            let features = Some(features);
            validate_stub_features(&features, &opts.required_features, &opts.demanding)?;
            return Ok(Resolved {
                bytes_path: path,
                origin: "fetch",
                sha256: sha,
                features,
            });
        }
        Err(crate::rtstub::fetch::FetchError::Integrity(m)) => {
            // INTEGRITY is FATAL — never fall through to a weaker rung.
            return Err(format!(
                "stub integrity failure (fetch): {m} — refusing (a tampered stub is never \
                 recovered by falling back to a local rung)"
            ));
        }
        Err(crate::rtstub::fetch::FetchError::Unavailable(m)) => {
            reasons.push(format!("fetch: {m}"));
        }
    }

    // Cross targets stop here — no local binary can serve a foreign triple.
    if !host {
        return Err(format!(
            "no stub available for target '{target}': {} — provide one with --stub <path> \
             (a pre-built ascript-rt for that target) or build one with --exact",
            reasons.join("; "),
        ));
    }

    // ── Rung 4: dev sibling — an `ascript-rt` beside current_exe() ──────────────────────
    match sibling_stub_path() {
        Some(sib) => {
            let (clean_path, sha) = materialize_clean_stub(&sib, "sibling")?;
            match probe_rt_info_features(&clean_path) {
                Ok(features) => {
                    let features = Some(features);
                    validate_stub_features(&features, &opts.required_features, &opts.demanding)?;
                    return Ok(Resolved {
                        bytes_path: clean_path,
                        origin: "sibling",
                        sha256: sha,
                        features,
                    });
                }
                Err(e) => reasons.push(format!("sibling: {e}")),
            }
        }
        None => reasons.push("sibling: no ascript-rt beside the toolchain binary".to_string()),
    }

    // ── Rung 5: current_exe() — today's exact behavior + a one-time warning ─────────────
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the running executable: {e}"))?;
    let (clean_path, sha) = materialize_clean_stub(&exe, "current-exe")?;
    warn_current_exe_fallback(&reasons);
    Ok(Resolved {
        bytes_path: clean_path,
        origin: "current_exe",
        // current_exe carries the FULL toolchain — its features cover every tier, so the §4.3
        // check is trivially satisfied; we report None (the report shows the selected tier
        // separately) to avoid implying the bundle is a trim stub.
        sha256: sha,
        features: None,
    })
}

/// The path to an `ascript-rt` sibling of `current_exe()` (rung 4), if it exists.
fn sibling_stub_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "ascript-rt.exe" } else { "ascript-rt" };
    let sib = dir.join(name);
    if sib.exists() && sib != exe {
        Some(sib)
    } else {
        None
    }
}

/// Emit the ONE-TIME rung-5 stderr warning (RT §5.4 rung 5): every earlier rung was
/// unavailable, so the bundle carries the full toolchain binary as its stub. Printed at most
/// once per process so a batch build does not spam.
fn warn_current_exe_fallback(reasons: &[String]) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    let why = if reasons.is_empty() {
        "no smaller stub was requested".to_string()
    } else {
        reasons.join("; ")
    };
    eprintln!(
        "warning: bundling onto the full toolchain binary (current_exe) — no runtime-only \
         stub was available ({why}). The bundle carries the whole ascript toolchain; provide \
         a smaller stub with --stub <ascript-rt> or build one with --exact."
    );
}
