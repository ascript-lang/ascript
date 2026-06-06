//! SP6 — package manager / dependency story (CLI-only, `#[cfg(feature = "pkg")]`).
//!
//! This module set lives entirely in the `ascript` binary behind the default-on
//! `pkg` Cargo feature, keeping TOML/IO out of the interpreter core (the lint
//! config TOML lives in `ascript::check::config_toml`). Nothing here is
//! reachable from `src/interp.rs` /
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

use ascript::interp::ResolvedPkg;
use manifest::{Manifest, PackageMeta};
use std::path::{Path, PathBuf};

/// Resolve a package directory's ENTRY module — the file a bare `import "<name>"`
/// binds (SP6 §2). Honors an explicit `[package].entry`, else tries the defaults
/// in order: `src/main.as` → `main.as` → `<name>.as`. Returns the absolute path
/// to the first that exists, or a clear error if none do.
pub fn resolve_entry(root: &Path, pkg: &PackageMeta) -> Result<PathBuf, String> {
    if let Some(entry) = &pkg.entry {
        let p = root.join(entry);
        if p.is_file() {
            return Ok(p);
        }
        return Err(format!(
            "package '{}' declares entry '{}' but {} does not exist",
            pkg.name,
            entry,
            p.display()
        ));
    }
    for cand in [
        PathBuf::from("src").join("main.as"),
        PathBuf::from("main.as"),
        PathBuf::from(format!("{}.as", leaf_name(&pkg.name))),
    ] {
        let p = root.join(&cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "package '{}' has no entry module (looked for src/main.as, main.as, {}.as in {})",
        pkg.name,
        leaf_name(&pkg.name),
        root.display()
    ))
}

/// The leaf of a package name for the `<name>.as` entry default: drop a leading
/// `@scope/` if present.
fn leaf_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Read a fetched/path package's manifest + resolve its entry into a
/// [`ResolvedPkg`]. `root` is the package's loadable root directory.
pub fn resolved_pkg_for(root: &Path) -> Result<ResolvedPkg, String> {
    let toml_path = root.join("ascript.toml");
    let text = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
    let manifest = Manifest::parse(&toml_path.display().to_string(), &text)?;
    let pkg = manifest.package.ok_or_else(|| {
        format!(
            "package at {} has no [package] table (required to be a dependency)",
            root.display()
        )
    })?;
    let entry = resolve_entry(root, &pkg)?;
    Ok(ResolvedPkg {
        root: root.to_path_buf(),
        entry,
    })
}

