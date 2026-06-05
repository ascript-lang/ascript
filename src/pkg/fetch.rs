//! Acquire path / git / url dependencies into the content-addressed store (SP6 §4).
//!
//! - **path** deps load IN PLACE (the package root IS the local dir); no copy, no
//!   integrity — the explicitly non-reproducible escape hatch.
//! - **git** deps use the `git` CLI subprocess (no git crate linked): bare
//!   clone/fetch into `cache/git/…`, checkout the `tag`/`rev` into `cache/tmp/`,
//!   `git rev-parse` for the resolved commit, then `asum1`-hash the staged tree
//!   and atomic-rename into `store/<hash>/`. A bare archive — NO hooks, NO
//!   submodule scripts (structurally no install scripts, D8).
//! - **url** deps download a tarball/zip, extract into `cache/tmp/`, read the
//!   package's `[package].version`, then hash + stage. A `file://` URL reads the
//!   archive from disk directly (used by the hermetic tests; no socket).
//!
//! After staging, the destination is `store/<asum1>/`. If it already exists
//! (another project fetched it) the staged copy is discarded — content-addressed
//! dedup. The move into `store/` is atomic (rename).

use super::cache;
use super::hash::asum1_tree;
use super::manifest::{DepSource, GitPin, Manifest, Version};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The outcome of fetching a single dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The loadable package root (a `store/<hash>/` dir for git/url; the local
    /// dir for a path dep).
    pub root: PathBuf,
    /// `asum1-…` integrity over the staged tree. `None` for a path dep
    /// (local + mutable — unhashed by design).
    pub integrity: Option<String>,
    /// The version MVS recorded as `resolved`: the tag/rev for git, the fetched
    /// package's `[package].version` for url, or the raw path string for a path
    /// dep.
    pub resolved: String,
    /// The exact commit a git dep resolved to (`git rev-parse`). `None` for
    /// url/path deps.
    pub rev: Option<String>,
}

/// Is the `git` CLI available on `PATH`? Tests skip the git arm (with a message)
/// when it is not, instead of failing.
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Fetch a dependency. `manifest_dir` is the directory of the manifest that
/// declared the dep (path deps resolve relative to it).
///
/// Async because the url arm awaits a `reqwest` download; the git/path arms are
/// synchronous (subprocess / filesystem) and run inline.
pub async fn fetch(src: &DepSource, manifest_dir: &Path) -> Result<Fetched, String> {
    match src {
        DepSource::Path { path } => fetch_path(path, manifest_dir),
        DepSource::Git { url, pin } => fetch_git(url, pin),
        DepSource::Url { url } => fetch_url(url).await,
        DepSource::Registry { req } => Err(format!(
            "bare-version dependency '{req}' requires a registry, which is not available yet"
        )),
    }
}

/// Path dep: the package root is the local directory (relative to the manifest
/// dir), loaded in place. No copy, no integrity.
fn fetch_path(path: &str, manifest_dir: &Path) -> Result<Fetched, String> {
    let joined = manifest_dir.join(path);
    let root = joined
        .canonicalize()
        .map_err(|e| format!("path dependency '{path}' not found: {e}"))?;
    if !root.is_dir() {
        return Err(format!("path dependency '{path}' is not a directory"));
    }
    Ok(Fetched {
        root,
        integrity: None,
        resolved: path.to_string(),
        rev: None,
    })
}

/// Git dep: bare clone/fetch, checkout the tag/rev, rev-parse, stage + hash.
fn fetch_git(url: &str, pin: &GitPin) -> Result<Fetched, String> {
    if !git_available() {
        return Err("the `git` binary is required to fetch git dependencies".to_string());
    }
    cache::create_dirs().map_err(|e| format!("cannot create cache dirs: {e}"))?;

    let bare = cache::git_dir(url);
    ensure_bare_clone(url, &bare)?;

    let refspec = match pin {
        GitPin::Tag(t) => t.clone(),
        GitPin::Rev(r) => r.clone(),
    };

    // Resolve the ref to a concrete commit for pinning + the `resolved` field.
    let rev = git_rev_parse(&bare, &refspec)?;

    // Stage the tree at that commit into a fresh tmp dir via `git archive` piped
    // through `tar` (no working tree, no hooks). Then hash + move into store/.
    let stage = unique_tmp("git")?;
    git_archive_into(&bare, &rev, &stage)?;

    let resolved = match pin {
        GitPin::Tag(t) => t.clone(),
        GitPin::Rev(r) => r.clone(),
    };

    let integrity = asum1_tree(&stage)?;
    let root = stage_into_store(&stage, &integrity)?;

    Ok(Fetched {
        root,
        integrity: Some(integrity),
        resolved,
        rev: Some(rev),
    })
}

/// Url dep: download (or read a `file://`), extract, read version, hash + stage.
async fn fetch_url(url: &str) -> Result<Fetched, String> {
    cache::create_dirs().map_err(|e| format!("cannot create cache dirs: {e}"))?;

    let bytes = if let Some(local) = url.strip_prefix("file://") {
        // Hermetic local-file fetch (tests): read the archive straight from disk.
        std::fs::read(local).map_err(|e| format!("cannot read url file {local}: {e}"))?
    } else {
        download(url).await?
    };

    let stage = unique_tmp("url")?;
    extract_archive(url, &bytes, &stage)?;

    // The extracted tree may be wrapped in a single top-level dir (the common
    // tarball shape); descend into it if the package root is one level down.
    let pkg_root = locate_package_root(&stage)?;

    // Read the fetched package's [package].version for the `resolved` field.
    let resolved = read_package_version(&pkg_root)?;

    let integrity = asum1_tree(&pkg_root)?;
    let root = stage_into_store(&pkg_root, &integrity)?;

    Ok(Fetched {
        root,
        integrity: Some(integrity),
        resolved: resolved.to_string(),
        rev: None,
    })
}

async fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("failed to download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {url} failed with status {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("failed to read {url}: {e}"))?;
    Ok(bytes.to_vec())
}

// ---- git subprocess helpers -------------------------------------------------

/// Create (or fetch into) the bare clone at `bare`. Reused across fetch/update.
fn ensure_bare_clone(url: &str, bare: &Path) -> Result<(), String> {
    if bare.join("HEAD").is_file() {
        // Existing bare repo: fetch all refs+tags to pick up new tags/commits.
        run_git(
            None,
            &[
                "--git-dir",
                &bare.to_string_lossy(),
                "fetch",
                "--tags",
                "--force",
                "origin",
                "+refs/heads/*:refs/heads/*",
            ],
        )?;
    } else {
        if let Some(parent) = bare.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create git cache dir: {e}"))?;
        }
        run_git(
            None,
            &["clone", "--bare", "--quiet", url, &bare.to_string_lossy()],
        )?;
    }
    Ok(())
}

/// Resolve a tag/rev/branch to a concrete commit hash within the bare repo.
fn git_rev_parse(bare: &Path, refspec: &str) -> Result<String, String> {
    // `<ref>^{commit}` peels an annotated tag to its commit.
    let out = git_output(
        &[
            "--git-dir",
            &bare.to_string_lossy(),
            "rev-parse",
            &format!("{refspec}^{{commit}}"),
        ],
    )
    .or_else(|_| {
        git_output(&[
            "--git-dir",
            &bare.to_string_lossy(),
            "rev-parse",
            refspec,
        ])
    })
    .map_err(|e| format!("cannot resolve git ref '{refspec}': {e}"))?;
    let rev = out.trim().to_string();
    if rev.is_empty() {
        return Err(format!("git ref '{refspec}' resolved to an empty commit"));
    }
    Ok(rev)
}

/// Stage the tree at `rev` into `dest` via `git archive | tar -x` (no working
/// tree, no hooks, no submodule scripts).
fn git_archive_into(bare: &Path, rev: &str, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create stage dir: {e}"))?;
    let archive = git_output_bytes(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "archive",
        "--format=tar",
        rev,
    ])?;
    let mut ar = tar::Archive::new(std::io::Cursor::new(archive));
    ar.unpack(dest)
        .map_err(|e| format!("cannot unpack git archive: {e}"))?;
    Ok(())
}

fn run_git(cwd: Option<&Path>, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn git_output(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn git_output_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

// ---- staging helpers --------------------------------------------------------

/// A unique staging dir under `cache/tmp/` (so concurrent fetches don't clash).
fn unique_tmp(tag: &str) -> Result<PathBuf, String> {
    let base = cache::tmp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base.join(format!("{tag}-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create stage dir: {e}"))?;
    Ok(dir)
}

/// Atomic-move `staged` into `store/<hash>/`. If the store entry already exists,
/// keep it (content-addressed dedup) and discard the staged copy.
fn stage_into_store(staged: &Path, hash: &str) -> Result<PathBuf, String> {
    let dest = cache::store_dir(hash);
    if dest.is_dir() {
        // Already present from a prior fetch: dedup. Best-effort cleanup of the
        // staged tree if it is a tmp dir (not the dedup target itself).
        if staged != dest {
            let _ = std::fs::remove_dir_all(staged);
        }
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create store dir: {e}"))?;
    }
    match std::fs::rename(staged, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            // Rename can fail across filesystems or if a racing fetch just won;
            // if the destination now exists, accept it (dedup), else copy.
            if dest.is_dir() {
                let _ = std::fs::remove_dir_all(staged);
                return Ok(dest);
            }
            copy_tree(staged, &dest)?;
            let _ = std::fs::remove_dir_all(staged);
            Ok(dest)
        }
    }
}

/// Recursive directory copy (cross-filesystem rename fallback).
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?
    {
        let entry = entry.map_err(|e| format!("cannot read dir entry: {e}"))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| format!("cannot stat: {e}"))?;
        if ft.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ft.is_file() {
            std::fs::copy(&src, &dst).map_err(|e| format!("cannot copy file: {e}"))?;
        }
    }
    Ok(())
}

// ---- url archive helpers ----------------------------------------------------

/// Extract a `.tar.gz`/`.tgz`/`.tar`/`.zip` archive (chosen by url suffix) into
/// `dest`.
fn extract_archive(url: &str, bytes: &[u8], dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create extract dir: {e}"))?;
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        let reader = std::io::Cursor::new(bytes);
        let mut zip =
            zip::ZipArchive::new(reader).map_err(|e| format!("invalid zip archive: {e}"))?;
        zip.extract(dest)
            .map_err(|e| format!("cannot extract zip: {e}"))?;
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        let dec = flate2::read::GzDecoder::new(bytes);
        let mut ar = tar::Archive::new(dec);
        ar.unpack(dest)
            .map_err(|e| format!("cannot extract tar.gz: {e}"))?;
    } else if lower.ends_with(".tar") {
        let mut ar = tar::Archive::new(std::io::Cursor::new(bytes));
        ar.unpack(dest)
            .map_err(|e| format!("cannot extract tar: {e}"))?;
    } else {
        return Err(format!(
            "unsupported archive type for url '{url}' (expected .tar.gz/.tgz/.tar/.zip)"
        ));
    }
    Ok(())
}

/// Find the package root inside an extracted archive: the dir containing an
/// `ascript.toml`. Handles both a flat archive and the common single-top-level-
/// dir wrapper. Errors if no `ascript.toml` is found within one level.
fn locate_package_root(extracted: &Path) -> Result<PathBuf, String> {
    if extracted.join("ascript.toml").is_file() {
        return Ok(extracted.to_path_buf());
    }
    // Look one level down (single wrapper dir).
    let mut subdirs = Vec::new();
    for entry in
        std::fs::read_dir(extracted).map_err(|e| format!("cannot read extracted tree: {e}"))?
    {
        let entry = entry.map_err(|e| format!("cannot read dir entry: {e}"))?;
        if entry.path().is_dir() {
            subdirs.push(entry.path());
        }
    }
    for d in &subdirs {
        if d.join("ascript.toml").is_file() {
            return Ok(d.clone());
        }
    }
    Err("downloaded archive does not contain an ascript.toml package".to_string())
}

/// Read `[package].version` from a fetched package's `ascript.toml`.
fn read_package_version(pkg_root: &Path) -> Result<Version, String> {
    let toml_path = pkg_root.join("ascript.toml");
    let text = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
    let manifest = Manifest::parse(&toml_path.display().to_string(), &text)?;
    let pkg = manifest
        .package
        .ok_or("fetched package has no [package] table")?;
    Ok(pkg.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fetch-test-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, rel: &str, contents: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    #[tokio::test]
    async fn path_dep_resolves_in_place_no_integrity() {
        let root = scratch("pathdep");
        let lib = root.join("lib");
        write(&lib, "main.as", "print(1)\n");
        write(
            &lib,
            "ascript.toml",
            "[package]\nname=\"lib\"\nversion=\"1.0.0\"\n",
        );

        let f = fetch(
            &DepSource::Path {
                path: "lib".to_string(),
            },
            &root,
        )
        .await
        .unwrap();
        assert_eq!(f.root, lib.canonicalize().unwrap());
        assert!(f.integrity.is_none(), "path deps have no integrity");
        assert_eq!(f.resolved, "lib");
        assert!(f.rev.is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn path_dep_missing_is_error() {
        let root = scratch("missingpath");
        let e = fetch(
            &DepSource::Path {
                path: "nope".to_string(),
            },
            &root,
        )
        .await
        .unwrap_err();
        assert!(e.contains("not found"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn registry_dep_needs_a_registry() {
        let e = fetch(
            &DepSource::Registry {
                req: "^1.2.0".to_string(),
            },
            Path::new("."),
        )
        .await
        .unwrap_err();
        assert!(e.contains("requires a registry"), "{e}");
    }

    /// A local `file://` git repo, created in a tempdir, fetched + checked out by
    /// tag. Hermetic (no network). Skips with a message if `git` is absent.
    #[tokio::test]
    async fn git_dep_local_file_repo_tag() {
        if !git_available() {
            eprintln!("SKIP git_dep_local_file_repo_tag: `git` binary not found");
            return;
        }
        // Isolate the cache for this test (serialize global-env mutation).
        let _g = cache::TEST_ENV_ALOCK.lock().await;
        let cache_dir = scratch("gitcache");
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", &cache_dir);

        // Build a source repo with a tagged commit.
        let work = scratch("gitwork");
        write(&work, "main.as", "print(\"v1\")\n");
        write(
            &work,
            "ascript.toml",
            "[package]\nname=\"demo\"\nversion=\"1.0.0\"\n",
        );
        let g = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&work)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        g(&["init", "-q"]);
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "v1"]);
        g(&["tag", "v1.0.0"]);

        let url = format!("file://{}", work.display());
        let f = fetch(
            &DepSource::Git {
                url,
                pin: GitPin::Tag("v1.0.0".to_string()),
            },
            Path::new("."),
        )
        .await
        .unwrap();

        assert!(f.integrity.as_deref().unwrap().starts_with("asum1-"));
        assert_eq!(f.resolved, "v1.0.0");
        assert_eq!(f.rev.as_ref().unwrap().len(), 40, "full sha");
        // The staged store tree has the package files.
        assert!(f.root.join("main.as").is_file());
        assert!(f.root.join("ascript.toml").is_file());
        // The store path is content-addressed by the integrity hash.
        assert!(f.root.ends_with(f.integrity.as_deref().unwrap()));

        match prev {
            Some(v) => std::env::set_var("ASCRIPT_CACHE", v),
            None => std::env::remove_var("ASCRIPT_CACHE"),
        }
        let _ = fs::remove_dir_all(&cache_dir);
        let _ = fs::remove_dir_all(&work);
    }

    /// A `file://` url tarball, hermetic (no socket). Builds a .tar.gz of a
    /// package, fetches it, asserts it stages with the package's version.
    #[tokio::test]
    async fn url_dep_local_file_tarball() {
        let _g = cache::TEST_ENV_ALOCK.lock().await;
        let cache_dir = scratch("urlcache");
        let prev = std::env::var_os("ASCRIPT_CACHE");
        std::env::set_var("ASCRIPT_CACHE", &cache_dir);

        // Build a package tree and tar.gz it (wrapped in a top-level dir).
        let pkg = scratch("urlpkg");
        write(&pkg, "as-parse/main.as", "print(\"parse\")\n");
        write(
            &pkg,
            "as-parse/ascript.toml",
            "[package]\nname=\"parse\"\nversion=\"1.2.0\"\n",
        );
        let tar_gz = {
            let enc =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all("as-parse", pkg.join("as-parse")).unwrap();
            let enc = builder.into_inner().unwrap();
            enc.finish().unwrap()
        };
        let archive_path = cache_dir.join("as-parse-1.2.0.tar.gz");
        fs::write(&archive_path, &tar_gz).unwrap();

        let url = format!("file://{}", archive_path.display());
        let f = fetch(&DepSource::Url { url }, Path::new(".")).await.unwrap();

        assert_eq!(f.resolved, "1.2.0", "from the fetched [package].version");
        assert!(f.integrity.as_deref().unwrap().starts_with("asum1-"));
        assert!(f.rev.is_none());
        assert!(f.root.join("main.as").is_file());
        assert!(f.root.join("ascript.toml").is_file());

        match prev {
            Some(v) => std::env::set_var("ASCRIPT_CACHE", v),
            None => std::env::remove_var("ASCRIPT_CACHE"),
        }
        let _ = fs::remove_dir_all(&cache_dir);
        let _ = fs::remove_dir_all(&pkg);
    }
}
