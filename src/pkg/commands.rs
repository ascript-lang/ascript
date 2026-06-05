//! The package-manager driver: ensure-lock for `run`/`test` and the `add` /
//! `remove` / `install` / `update` / `lock` / `tree` / `verify` CLI commands
//! (SP6 §3/§5/§7).

use super::cache;
use super::fetch::{self, Fetched};
use super::hash::asum1_tree;
use super::lock::{LockEntry, Lockfile};
use super::manifest::{DepSource, GitPin, Manifest};
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

/// Ensure the lock is satisfied for the program at `entry_file`, returning the
/// resolved [`PackageMap`] to inject onto the interpreter (SP6 §7). `locked` =
/// offline-deterministic against the existing `ascript.lock` (fail on drift /
/// missing lock / integrity mismatch). Online mode resolves via MVS (fetch on
/// miss) and writes/refreshes the lock beside the manifest.
///
/// `Ok(None)` means there is no manifest (a bare script with no deps) — the
/// caller installs no resolver.
pub fn ensure_lock(entry_file: &Path, locked: bool) -> Result<Option<PackageMap>, String> {
    let Some((manifest_dir, manifest)) = Manifest::load_nearest(entry_file)? else {
        return Ok(None);
    };
    if manifest.dependencies.is_empty() {
        return Ok(Some(PackageMap::new()));
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
    Ok(Some(map))
}

// ===== CLI commands (§7) =====================================================

/// Locate the nearest `ascript.toml` for a CLI command run from the cwd. All
/// package commands operate on it (and write `ascript.lock` beside it).
fn nearest_manifest() -> Result<(PathBuf, Manifest), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read cwd: {e}"))?;
    // `discover` anchors on the file's parent; pass a sentinel file in cwd.
    let probe = cwd.join("__ascript_probe__");
    Manifest::load_nearest(&probe)?
        .ok_or_else(|| "no ascript.toml found (run from a project directory)".to_string())
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("ascript.toml")
}

/// Parse an `add` spec into a `(name, DepSource)` (§7).
///
/// - `../path` or an absolute path → a path dep (name = last path component).
/// - `github.com/owner/repo@tag` → `git+https://github.com/owner/repo`, tag.
/// - `https://…@tag` / `http://…@tag` / `file://…@tag` → git+url, tag.
/// - `https://….tar.gz` (no `@tag`) → a url tarball dep.
/// - a bare name (`color`) → the reserved-future registry error.
pub fn parse_add_spec(spec: &str) -> Result<(String, DepSource), String> {
    // Path dep.
    if spec.starts_with("./") || spec.starts_with("../") || Path::new(spec).is_absolute() {
        let name = Path::new(spec)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("cannot derive a package name from path '{spec}'"))?
            .to_string();
        return Ok((name, DepSource::Path { path: spec.to_string() }));
    }

    // git@tag form: a url/host with a trailing `@<tag>`.
    if let Some((base, tag)) = spec.rsplit_once('@') {
        // Avoid mis-splitting an scp-style git url (user@host) — only treat as a
        // tag split when the base looks like a url/host with a path.
        if base.contains('/') {
            let url = normalize_git_url(base);
            let name = git_repo_name(&url);
            return Ok((
                name,
                DepSource::Git {
                    url,
                    pin: GitPin::Tag(tag.to_string()),
                },
            ));
        }
    }

    // A plain url tarball.
    if spec.starts_with("http://") || spec.starts_with("https://") || spec.starts_with("file://") {
        if spec.ends_with(".tar.gz") || spec.ends_with(".tgz") || spec.ends_with(".tar")
            || spec.ends_with(".zip")
        {
            let name = url_archive_name(spec)?;
            return Ok((name, DepSource::Url { url: spec.to_string() }));
        }
        // A bare git url with no @tag: ambiguous — require an explicit @tag.
        return Err(format!(
            "git dependency '{spec}' needs an @tag (e.g. '{spec}@v1.0.0')"
        ));
    }

    // `github.com/owner/repo` (no scheme) → git+https.
    if spec.contains('/') && !spec.contains("://") {
        return Err(format!(
            "git dependency '{spec}' needs an @tag (e.g. '{spec}@v1.0.0')"
        ));
    }

    // A bare name → the reserved-future registry source.
    Err(format!(
        "bare-name dependency '{spec}' requires a registry, which is not available yet \
         (use a git/url/path spec, e.g. 'github.com/owner/repo@v1.0.0' or '../{spec}')"
    ))
}

/// Infer `git+https://` for a scheme-less host spec (`github.com/owner/repo`).
fn normalize_git_url(base: &str) -> String {
    if base.contains("://") {
        base.to_string()
    } else {
        format!("https://{base}")
    }
}

/// The repo name (last path segment, minus a trailing `.git`) of a git url.
fn git_repo_name(url: &str) -> String {
    let after = url.rsplit('/').next().unwrap_or(url);
    after.strip_suffix(".git").unwrap_or(after).to_string()
}

/// Derive a package name from a url-tarball filename (strip the version + ext).
fn url_archive_name(url: &str) -> Result<String, String> {
    let file = url.rsplit('/').next().unwrap_or(url);
    let stem = file
        .strip_suffix(".tar.gz")
        .or_else(|| file.strip_suffix(".tgz"))
        .or_else(|| file.strip_suffix(".tar"))
        .or_else(|| file.strip_suffix(".zip"))
        .unwrap_or(file);
    // Drop a trailing `-X.Y.Z` version if present.
    let name = match stem.rsplit_once('-') {
        Some((n, ver)) if ver.split('.').all(|p| p.bytes().all(|b| b.is_ascii_digit())) && !n.is_empty() => n,
        _ => stem,
    };
    if name.is_empty() {
        return Err(format!("cannot derive a package name from url '{url}'"));
    }
    Ok(name.to_string())
}

/// Render a `[dependencies]` value as the inline TOML it should appear as.
fn dep_toml_value(src: &DepSource) -> String {
    match src {
        DepSource::Git { url, pin: GitPin::Tag(t) } => {
            format!("{{ git = \"{url}\", tag = \"{t}\" }}")
        }
        DepSource::Git { url, pin: GitPin::Rev(r) } => {
            format!("{{ git = \"{url}\", rev = \"{r}\" }}")
        }
        DepSource::Url { url } => format!("{{ url = \"{url}\" }}"),
        DepSource::Path { path } => format!("{{ path = \"{path}\" }}"),
        DepSource::Registry { req } => format!("\"{req}\""),
    }
}

/// Add (or replace) a `[dependencies]` entry in the manifest text, preserving the
/// rest of the file. Creates the `[dependencies]` table if absent.
fn manifest_upsert_dep(text: &str, name: &str, src: &DepSource) -> String {
    let line = format!("{name} = {}", dep_toml_value(src));
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();

    // Find the [dependencies] header and the existing entry (if any).
    let header_idx = lines.iter().position(|l| l.trim() == "[dependencies]");
    let key_prefix = format!("{name} =");
    if let Some(hidx) = header_idx {
        // Replace an existing entry within the table, else append after header.
        let mut replaced = false;
        for l in lines.iter_mut().skip(hidx + 1) {
            if l.trim_start().starts_with('[') {
                break; // next table
            }
            if l.trim_start().starts_with(&key_prefix) {
                *l = line.clone();
                replaced = true;
                break;
            }
        }
        if !replaced {
            lines.insert(hidx + 1, line);
        }
    } else {
        // No table yet: append one.
        if !text.is_empty() && !text.ends_with('\n') {
            lines.push(String::new());
        }
        lines.push(String::new());
        lines.push("[dependencies]".to_string());
        lines.push(line);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Remove a `[dependencies]` entry from the manifest text. Returns `Ok(false)` if
/// the entry was not present.
fn manifest_remove_dep(text: &str, name: &str) -> (String, bool) {
    let key_prefix = format!("{name} =");
    let mut removed = false;
    let mut in_deps = false;
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let trimmed = l.trim_start();
        if trimmed == "[dependencies]" {
            in_deps = true;
            out.push(l.to_string());
            continue;
        }
        if trimmed.starts_with('[') {
            in_deps = false;
        }
        if in_deps && trimmed.starts_with(&key_prefix) {
            removed = true;
            continue; // drop this line
        }
        out.push(l.to_string());
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    (s, removed)
}

/// Re-resolve + write the lock for `manifest_dir` (online). Shared by add /
/// remove / install / update / lock.
fn relock(manifest_dir: &Path) -> Result<Vec<Resolved>, String> {
    let manifest = read_manifest(manifest_dir)?;
    let root_deps = deps_vec(&manifest);
    let mut fetcher = OnlineFetcher;
    let resolved = resolve::resolve(&root_deps, manifest_dir, &mut fetcher)?;
    let lock = Lockfile {
        version: super::lock::LOCK_VERSION,
        packages: resolved.iter().map(lock_entry).collect(),
    };
    lock.write_beside(manifest_dir)?;
    Ok(resolved)
}

/// `ascript add <spec>`: add a dep to the manifest, then re-resolve + re-lock.
pub fn cmd_add(spec: &str) -> Result<(), String> {
    let (dir, _manifest) = nearest_manifest()?;
    let (name, src) = parse_add_spec(spec)?;
    let path = manifest_path(&dir);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let updated = manifest_upsert_dep(&text, &name, &src);
    std::fs::write(&path, updated).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    relock(&dir)?;
    println!("added '{name}' ({})", source_string(&src));
    Ok(())
}

/// `ascript remove <name>`: drop a dep from the manifest, then re-lock.
pub fn cmd_remove(name: &str) -> Result<(), String> {
    let (dir, _manifest) = nearest_manifest()?;
    let path = manifest_path(&dir);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (updated, removed) = manifest_remove_dep(&text, name);
    if !removed {
        return Err(format!("dependency '{name}' is not in {}", path.display()));
    }
    std::fs::write(&path, updated).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    relock(&dir)?;
    println!("removed '{name}'");
    Ok(())
}

/// `ascript install [--locked]`: resolve + fetch + write/verify the lock.
pub fn cmd_install(locked: bool) -> Result<(), String> {
    let (dir, manifest) = nearest_manifest()?;
    if locked {
        // Offline: resolve against the existing lock + verify integrity.
        let lock = Lockfile::read_beside(&dir)?.ok_or_else(|| {
            "--locked: no ascript.lock found (run `ascript install` first)".to_string()
        })?;
        let mut fetcher = LockedFetcher { lock };
        let resolved = resolve::resolve(&deps_vec(&manifest), &dir, &mut fetcher)?;
        println!("installed {} package(s) from ascript.lock", resolved.len());
    } else {
        let resolved = relock(&dir)?;
        println!("installed {} package(s)", resolved.len());
    }
    Ok(())
}

/// `ascript lock`: (re)generate the lock from the manifest (online where needed).
pub fn cmd_lock() -> Result<(), String> {
    let (dir, _manifest) = nearest_manifest()?;
    let resolved = relock(&dir)?;
    println!("locked {} package(s)", resolved.len());
    Ok(())
}

/// `ascript update [name]`: re-resolve (online), raising pins to the newest
/// satisfying tag where the manifest allows, and rewrite the lock. SP6's MVS is
/// reproducible-by-default, so `update` simply re-resolves the manifest as
/// declared (a manifest tag bump is the explicit knob); a `name` filter is
/// accepted but currently re-locks the whole graph.
pub fn cmd_update(name: Option<&str>) -> Result<(), String> {
    let (dir, _manifest) = nearest_manifest()?;
    let resolved = relock(&dir)?;
    match name {
        Some(n) => println!("updated (re-locked {} package(s); filter '{n}')", resolved.len()),
        None => println!("updated (re-locked {} package(s))", resolved.len()),
    }
    Ok(())
}

/// `ascript tree`: print the resolved dependency graph (name, resolved, source).
pub fn cmd_tree() -> Result<(), String> {
    let (dir, manifest) = nearest_manifest()?;
    // Resolve offline against the lock if present, else online.
    let resolved = match Lockfile::read_beside(&dir)? {
        Some(lock) => {
            let mut fetcher = LockedFetcher { lock };
            resolve::resolve(&deps_vec(&manifest), &dir, &mut fetcher)?
        }
        None => relock(&dir)?,
    };
    if resolved.is_empty() {
        println!("(no dependencies)");
        return Ok(());
    }
    for r in &resolved {
        println!("{} {} ({})", r.name, r.resolved, source_string(&r.source));
    }
    Ok(())
}

/// `ascript verify`: re-hash every non-path store entry against the lock
/// integrity. Fail-closed: non-zero on any mismatch or missing store entry.
pub fn cmd_verify() -> Result<(), String> {
    let (dir, _manifest) = nearest_manifest()?;
    let lock = Lockfile::read_beside(&dir)?
        .ok_or_else(|| "no ascript.lock to verify (run `ascript install` first)".to_string())?;
    let mut ok = 0usize;
    let mut unverified = 0usize;
    for entry in &lock.packages {
        match &entry.integrity {
            None => {
                // A path dep is unverified (local + mutable) by design.
                println!("{}: unverified (local path)", entry.name);
                unverified += 1;
            }
            Some(integrity) => {
                let root = cache::store_dir(integrity);
                if !root.is_dir() {
                    return Err(format!(
                        "'{}' ({integrity}) is not in the cache; run `ascript install`",
                        entry.name
                    ));
                }
                verify_integrity(&entry.name, &root, integrity)?;
                println!("{}: ok ({integrity})", entry.name);
                ok += 1;
            }
        }
    }
    println!("verified {ok} package(s); {unverified} unverified (path)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_spec_path() {
        let (n, s) = parse_add_spec("../util").unwrap();
        assert_eq!(n, "util");
        assert_eq!(s, DepSource::Path { path: "../util".into() });
    }

    #[test]
    fn add_spec_github_at_tag_infers_https() {
        let (n, s) = parse_add_spec("github.com/acme/as-http@v1.4.0").unwrap();
        assert_eq!(n, "as-http");
        assert_eq!(
            s,
            DepSource::Git {
                url: "https://github.com/acme/as-http".into(),
                pin: GitPin::Tag("v1.4.0".into())
            }
        );
    }

    #[test]
    fn add_spec_https_at_tag() {
        let (n, s) = parse_add_spec("https://example.com/acme/repo@v2.0.0").unwrap();
        assert_eq!(n, "repo");
        assert!(matches!(s, DepSource::Git { pin: GitPin::Tag(t), .. } if t == "v2.0.0"));
    }

    #[test]
    fn add_spec_url_tarball() {
        let (n, s) = parse_add_spec("https://example.com/as-parse-1.2.0.tar.gz").unwrap();
        assert_eq!(n, "as-parse", "version stripped");
        assert!(matches!(s, DepSource::Url { .. }));
    }

    #[test]
    fn add_spec_bare_name_needs_registry() {
        let e = parse_add_spec("color").unwrap_err();
        assert!(e.contains("requires a registry"), "{e}");
    }

    #[test]
    fn add_spec_bare_url_needs_tag() {
        let e = parse_add_spec("https://example.com/acme/repo").unwrap_err();
        assert!(e.contains("needs an @tag"), "{e}");
    }

    #[test]
    fn upsert_into_existing_table() {
        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n";
        let out = manifest_upsert_dep(
            text,
            "http",
            &DepSource::Git {
                url: "https://x/h".into(),
                pin: GitPin::Tag("v1.0.0".into()),
            },
        );
        assert!(out.contains("lib = { path = \"../lib\" }"));
        assert!(out.contains("http = { git = \"https://x/h\", tag = \"v1.0.0\" }"));
        // Round-trips through the manifest parser.
        let m = Manifest::parse("ascript.toml", &out).unwrap();
        assert!(m.dependencies.contains_key("http") && m.dependencies.contains_key("lib"));
    }

    #[test]
    fn upsert_replaces_existing_entry() {
        let text = "[dependencies]\nlib = { path = \"../old\" }\n";
        let out = manifest_upsert_dep(text, "lib", &DepSource::Path { path: "../new".into() });
        assert!(out.contains("../new"));
        assert!(!out.contains("../old"));
        assert_eq!(out.matches("lib =").count(), 1, "no duplicate entry");
    }

    #[test]
    fn upsert_creates_table_when_absent() {
        let text = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
        let out = manifest_upsert_dep(text, "lib", &DepSource::Path { path: "../lib".into() });
        assert!(out.contains("[dependencies]"));
        assert!(out.contains("lib = { path = \"../lib\" }"));
        let m = Manifest::parse("ascript.toml", &out).unwrap();
        assert!(m.dependencies.contains_key("lib"));
    }

    #[test]
    fn remove_drops_entry() {
        let text = "[dependencies]\nlib = { path = \"../lib\" }\nhttp = { path = \"../http\" }\n";
        let (out, removed) = manifest_remove_dep(text, "lib");
        assert!(removed);
        assert!(!out.contains("lib ="));
        assert!(out.contains("http ="));
    }

    #[test]
    fn remove_absent_is_false() {
        let text = "[dependencies]\nhttp = { path = \"../http\" }\n";
        let (_out, removed) = manifest_remove_dep(text, "nope");
        assert!(!removed);
    }

    #[test]
    fn source_string_prefixes() {
        assert_eq!(
            source_string(&DepSource::Git {
                url: "u".into(),
                pin: GitPin::Tag("v1.0.0".into())
            }),
            "git+u"
        );
        assert_eq!(source_string(&DepSource::Url { url: "u".into() }), "url+u");
        assert_eq!(source_string(&DepSource::Path { path: "../p".into() }), "path+../p");
    }
}
