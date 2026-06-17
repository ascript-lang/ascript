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
