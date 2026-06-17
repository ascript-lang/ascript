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

pub mod report;
pub mod select;
pub mod std_features;
pub mod tiers;
