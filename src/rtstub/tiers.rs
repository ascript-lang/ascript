//! RT §3.2 / §4.4 — the stub tier matrix and nearest-superset selection.
//!
//! The four tiers form a strict cumulative SUPERSET CHAIN:
//!
//! ```text
//! rt-core ⊊ rt-local ⊊ rt-net ⊊ rt-full
//! ```
//!
//! Each tier's feature set is a proper superset of the previous one. A chain makes
//! nearest-superset selection (§4.4) total and unambiguous: the chosen tier is the
//! FIRST in the chain whose feature set ⊇ the program's required features.
//!
//! **Single source of truth, drift-tested:** [`Tier::features`] is tested against
//! `scripts/build-rt.sh`'s `case`-arm feature lists (one source of truth checked against
//! the other, both directions — see `tests/rt_select.rs` `tier_drift_against_build_script`).
//!
//! Compiled only in the TOOLCHAIN build (`#[cfg(not(ascript_rt))]`): the stub already
//! has its features compiled in and never needs this table. Must build under
//! `--no-default-features` (the test-matrix contract) — nothing here depends on an
//! optional Cargo feature.

use std::collections::BTreeSet;

/// The four prebuilt stub tiers (RT §3.2). Listed in chain order: each is a proper
/// superset of the previous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// `shared`, `bundle-zstd` — pure-compute CLI tools.
    RtCore,
    /// rt-core + the local-machine batteries (data/binary/log/workflow/datetime/
    /// crypto/compress/sys/sysinfo/sql/tui).
    RtLocal,
    /// rt-local + the network stack (net/postgres/redis/telemetry).
    RtNet,
    /// rt-net + the heavies (intl/ai/ffi) — everything runtime-shaped.
    RtFull,
}

impl Tier {
    /// The tiers in chain order (rt-core first).
    pub const CHAIN: [Tier; 4] = [Tier::RtCore, Tier::RtLocal, Tier::RtNet, Tier::RtFull];

    /// The CUMULATIVE Cargo feature set this tier's stub is built with.
    ///
    /// MUST match `scripts/build-rt.sh`'s `case` arms exactly (drift-tested both ways).
    /// `bundle-zstd` is the stub-side payload decompressor (§3.1) — present in every tier.
    pub const fn features(self) -> &'static [&'static str] {
        match self {
            // rt-core
            Tier::RtCore => &["shared", "bundle-zstd"],
            // rt-local = rt-core + local batteries
            Tier::RtLocal => &[
                "shared", "bundle-zstd", "data", "binary", "log", "workflow", "datetime",
                "crypto", "compress", "sys", "sysinfo", "sql", "tui",
            ],
            // rt-net = rt-local + network stack
            Tier::RtNet => &[
                "shared", "bundle-zstd", "data", "binary", "log", "workflow", "datetime",
                "crypto", "compress", "sys", "sysinfo", "sql", "tui", "net", "postgres",
                "redis", "telemetry",
            ],
            // rt-full = rt-net + the heavies
            Tier::RtFull => &[
                "shared", "bundle-zstd", "data", "binary", "log", "workflow", "datetime",
                "crypto", "compress", "sys", "sysinfo", "sql", "tui", "net", "postgres",
                "redis", "telemetry", "intl", "ai", "ffi",
            ],
        }
    }

    /// The canonical name (`rt-core`/`rt-local`/`rt-net`/`rt-full`).
    pub const fn name(self) -> &'static str {
        match self {
            Tier::RtCore => "rt-core",
            Tier::RtLocal => "rt-local",
            Tier::RtNet => "rt-net",
            Tier::RtFull => "rt-full",
        }
    }

    /// Parse a tier name (the `--tier` CLI value). Returns `None` for an unknown name.
    pub fn parse(name: &str) -> Option<Tier> {
        match name {
            "rt-core" => Some(Tier::RtCore),
            "rt-local" => Some(Tier::RtLocal),
            "rt-net" => Some(Tier::RtNet),
            "rt-full" => Some(Tier::RtFull),
            _ => None,
        }
    }

    /// This tier's feature set as a `BTreeSet` for set-algebra (containment, diff).
    pub fn feature_set(self) -> BTreeSet<&'static str> {
        self.features().iter().copied().collect()
    }
}

/// Nearest-superset selection (§4.4): the FIRST tier in the chain whose feature set
/// ⊇ `required`. Because the chain is a cumulative superset chain (and `rt-full` is the
/// runtime maximum), there is always at least one satisfying tier for any subset of
/// rt-full's features; `required` only ever holds runtime features (from
/// [`required_features`](super::std_features::required_features)), so this is total.
pub fn select_tier(required: &BTreeSet<&str>) -> Tier {
    for tier in Tier::CHAIN {
        if required.iter().all(|f| tier.feature_set().contains(f)) {
            return tier;
        }
    }
    // rt-full is the runtime maximum; `required` never names a non-runtime feature, so
    // the loop above always returns. This is the defensive floor.
    Tier::RtFull
}

/// Validate that a FORCED tier (`--tier`) still satisfies the program's requirements
/// (§4.4 — a downward override only passes if it still covers `required`). On failure,
/// the error lists BOTH the missing features AND the modules that demand them, so the
/// user knows what to remove or which tier to pick.
///
/// `demanding_modules` maps each std import specifier to the feature(s) it needs, used
/// to attribute the missing features back to their source modules.
pub fn validate_forced_tier(
    forced: Tier,
    required: &BTreeSet<&str>,
    demanding_modules: &[(String, &'static str)],
) -> Result<(), String> {
    let have = forced.feature_set();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|f| !have.contains(f))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    // Attribute each missing feature back to the modules that demand it.
    let mut attributions: Vec<String> = Vec::new();
    for &feat in &missing {
        let mut mods: Vec<&str> = demanding_modules
            .iter()
            .filter(|(_, f)| *f == feat)
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
        "tier '{}' is insufficient: it is missing {} that the program requires — {}. \
         Pick a higher tier (e.g. '{}') or omit --tier for automatic selection.",
        forced.name(),
        if missing.len() == 1 { "the feature" } else { "the features" },
        attributions.join("; "),
        select_tier(required).name(),
    ))
}
