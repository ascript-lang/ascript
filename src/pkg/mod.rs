//! SP6 — package manager / dependency story (CLI-only, `#[cfg(feature = "pkg")]`).
//!
//! This module set lives entirely in the `ascript` binary behind the default-on
//! `pkg` Cargo feature, mirroring how `src/lint_config_toml.rs` keeps TOML/IO out
//! of the interpreter core. Nothing here is reachable from `src/interp.rs` /
//! `src/vm/*` / `src/value.rs`: the ONLY core change SP6 makes is the
//! dependency-free package-resolver map (`Interp::set_package_resolver`) plus the
//! shared `classify_specifier` helper, both of which use plain `std` types so the
//! core still builds (and a bare `import "x"` cleanly errors) under
//! `--no-default-features`.
//!
//! Layout (one concern per file):
//! - [`manifest`] — parse `ascript.toml` `[package]` + `[dependencies]`.
//! - [`cache`] — `$ASCRIPT_CACHE` / XDG / per-OS cache dir + store/git/tmp layout.
//! - [`hash`] — the `asum1` normalized-tree content hash (fail-closed integrity).
//! - [`fetch`] — acquire path / git / url deps into the content-addressed store.
//! - [`lock`] — read/write the committed `ascript.lock`.
//! - [`resolve`] — Go-style Minimal Version Selection over the dependency graph.
//! - [`commands`] — the `add`/`remove`/`install`/`update`/`lock`/`tree`/`verify`
//!   CLI commands.

pub mod cache;
pub mod commands;
pub mod fetch;
pub mod hash;
pub mod lock;
pub mod manifest;
pub mod resolve;
