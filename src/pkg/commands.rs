//! The package-manager driver: ensure-lock for `run`/`test` and the `add` /
//! `remove` / `install` / `update` / `lock` / `tree` / `verify` CLI commands
//! (SP6 §3/§5/§7).

use super::cache;
use super::fetch::{self, Fetched};
use super::hash::asum1_tree;
use super::lock::{LockEntry, Lockfile};
use super::manifest::{DepSource, Manifest};
use super::resolve::{self, DepFetcher, FetchedDep, Resolved};
use ascript::interp::PackageMap;
use std::path::{Path, PathBuf};

/// The online fetcher (fetch-on-miss into the cache). Resolves a path dep against
/// the requiring package's `base_dir`.
struct OnlineFetcher;

impl DepFetcher for OnlineFetcher {
    fn fetch(&mut self, name: &str, src: &DepSource, base_dir: &Path) -> Result<FetchedDep, String> {
        let fetched = fetch::fetch_blocking(src, base_dir)?;
        read_transitive(name, &fetched)
    }
}

/// The offline (`--locked`) fetcher: NO network. A git/url dep must already be in
/// the content-addressed store (keyed by the lock `integrity`); a path dep loads
/// in place. Re-hashes the store tree and FAILS on an integrity mismatch.
struct LockedFetcher {
    lock: Lockfile,
}

impl DepFetcher for LockedFetcher {
    fn fetch(&mut self, name: &str, src: &DepSource, base_dir: &Path) -> Result<FetchedDep, String> {
        match src {
            DepSource::Path { .. } => {
                // Path deps are always loaded in place (no lock integrity).
                let fetched = fetch::fetch_blocking(src, base_dir)?;
                read_transitive(name, &fetched)
            }
            DepSource::Registry { req } => Err(format!(
                "bare-version dependency '{req}' requires a registry, which is not available yet"
            )),
            _ => {
                // git/url: must be locked + present in the store, integrity-verified.
                let entry = self.lock.packages.iter().find(|e| e.name == name).ok_or_else(|| {
                    format!(
                        "--locked: '{name}' is not in ascript.lock (run `ascript install` to update)"
                    )
                })?;
                let integrity = entry.integrity.clone().ok_or_else(|| {
                    format!("--locked: lock entry '{name}' has no integrity")
                })?;
                let root = cache::store_dir(&integrity);
                if !root.is_dir() {
                    return Err(format!(
                        "--locked: '{name}' ({integrity}) is not in the cache; run \
                         `ascript install` (online) first"
                    ));
                }
                verify_integrity(name, &root, &integrity)?;
                let manifest = read_manifest(&root)?;
                Ok(FetchedDep {
                    resolved: entry.resolved.clone(),
                    rev: entry.rev.clone(),
                    integrity: Some(integrity),
                    root,
                    deps: deps_vec(&manifest),
                })
            }
        }
    }
}

/// Read a fetched package's transitive deps + carry forward its fetch metadata.
fn read_transitive(_name: &str, fetched: &Fetched) -> Result<FetchedDep, String> {
    let manifest = read_manifest(&fetched.root)?;
    Ok(FetchedDep {
        resolved: fetched.resolved.clone(),
        rev: fetched.rev.clone(),
        integrity: fetched.integrity.clone(),
        root: fetched.root.clone(),
        deps: deps_vec(&manifest),
    })
}

/// Read + parse the `ascript.toml` at a package root.
fn read_manifest(root: &Path) -> Result<Manifest, String> {
    let toml_path = root.join("ascript.toml");
    let text = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
    Manifest::parse(&toml_path.display().to_string(), &text)
}

/// A manifest's `[dependencies]` as an ordered name→source vec.
fn deps_vec(manifest: &Manifest) -> Vec<(String, DepSource)> {
    manifest
        .dependencies
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Re-hash a store tree and compare to the expected `asum1-…`. Fail-closed.
fn verify_integrity(name: &str, root: &Path, expected: &str) -> Result<(), String> {
    let actual = asum1_tree(root)?;
    if actual != expected {
        return Err(format!(
            "integrity mismatch for '{name}': expected {expected}, got {actual} \
             (the cached package was tampered with or corrupted)"
        ));
    }
    Ok(())
}

/// Build a [`LockEntry`] from a resolved package.
fn lock_entry(r: &Resolved) -> LockEntry {
    LockEntry {
        name: r.name.clone(),
        source: source_string(&r.source),
        requirement: r.requirement.clone(),
        resolved: r.resolved.clone(),
        rev: r.rev.clone(),
        integrity: r.integrity.clone(),
    }
}

/// Prefixed `source` string for the lockfile (`git+`/`url+`/`path+`).
fn source_string(src: &DepSource) -> String {
    match src {
        DepSource::Git { url, .. } => format!("git+{url}"),
        DepSource::Url { url } => format!("url+{url}"),
        DepSource::Path { path } => format!("path+{path}"),
        DepSource::Registry { req } => format!("registry+{req}"),
    }
}

/// Assemble the [`PackageMap`] (name → resolved root+entry) for the engine from a
/// resolved set. Each package's entry module is resolved via `resolved_pkg_for`.
fn package_map(resolved: &[Resolved]) -> Result<PackageMap, String> {
    let mut map = PackageMap::new();
    for r in resolved {
        let pkg = super::resolved_pkg_for(&r.root)?;
        map.insert(r.name.clone(), pkg);
    }
    Ok(map)
}

/// The outcome of ensuring the lock is satisfied for a `run`/`test`.
pub struct EnsureOutcome {
    /// The resolved package map for the engine.
    pub map: PackageMap,
    /// The directory of the nearest manifest (where `ascript.lock` lives).
    pub manifest_dir: PathBuf,
}

/// Ensure the lock is satisfied for the program at `entry_file`, returning the
/// resolved [`PackageMap`] to inject onto the interpreter (SP6 §7). `locked` =
/// offline-deterministic against the existing `ascript.lock` (fail on drift /
/// missing lock / integrity mismatch). Online mode resolves via MVS (fetch on
/// miss) and writes/refreshes the lock.
///
/// `Ok(None)` means there is no manifest (a bare script with no deps) — the
/// caller installs no resolver.
pub fn ensure_lock(entry_file: &Path, locked: bool) -> Result<Option<EnsureOutcome>, String> {
    let Some((manifest_dir, manifest)) = Manifest::load_nearest(entry_file)? else {
        return Ok(None);
    };
    if manifest.dependencies.is_empty() {
        return Ok(Some(EnsureOutcome {
            map: PackageMap::new(),
            manifest_dir,
        }));
    }
    let root_deps = deps_vec(&manifest);

    let resolved = if locked {
        let lock = Lockfile::read_beside(&manifest_dir)?.ok_or_else(|| {
            "--locked: no ascript.lock found (run `ascript install` first)".to_string()
        })?;
        let mut fetcher = LockedFetcher { lock };
        resolve::resolve(&root_deps, &manifest_dir, &mut fetcher)?
    } else {
        let mut fetcher = OnlineFetcher;
        let resolved = resolve::resolve(&root_deps, &manifest_dir, &mut fetcher)?;
        // Write/refresh the lock with the resolved set.
        let lock = Lockfile {
            version: super::lock::LOCK_VERSION,
            packages: resolved.iter().map(lock_entry).collect(),
        };
        lock.write_beside(&manifest_dir)?;
        resolved
    };

    let map = package_map(&resolved)?;
    Ok(Some(EnsureOutcome { map, manifest_dir }))
}
