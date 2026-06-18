//! RT §4.1–§4.2 — Module→feature table, archive std-import scanner, and the
//! feature-dependency closure.
//!
//! Compiled only in the TOOLCHAIN build (`#[cfg(not(ascript_rt))]`): this data
//! is used by `ascript build --native` to select the right stub tier. The stub
//! (ascript-rt) itself never needs it at runtime — it already has the right
//! features compiled in.
//!
//! Must also build under `--no-default-features` (the test matrix contract).
//! Nothing here may depend on an optional Cargo feature.

pub mod exact;
pub mod report;
pub mod select;
pub mod std_features;
pub mod tiers;

// RT §5.1–§5.3 — distribution: signed manifest, fail-closed fetch, content-addressed
// cache. The CACHE and the manifest PARSER build under `--no-default-features` (they
// need only `std` + the core `sha2`); only the signature-verify (`verify_manifest`) and
// the reqwest network arm inside `fetch` are gated on the default-on `rt-fetch` feature.
pub mod cache;
pub mod fetch;
pub mod manifest;

// RT §8 — `--oci` OCI image tarball writer. Gated on `compress` (flate2 for gzip).
// Decision recorded: `compress` is the gate because it provides flate2; sha2 is core;
// serde_json is also in the default set — the gate is the minimum required dep addition.
// Under `--no-default-features` the module still exists but all items are `#[cfg(...)]`-
// gated INSIDE the file — the `pub mod` declaration here is always present.
#[cfg(feature = "compress")]
pub mod oci;
