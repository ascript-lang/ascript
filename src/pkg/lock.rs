//! `ascript.lock` read/write (SP6 §3) — committed, human-diffable, stable.
//!
//! One `[[package]]` array entry per resolved package, SORTED by `name` for
//! stable diffs. The file carries its OWN format `version` counter (starts at
//! `1`, independent of `ASO_FORMAT_VERSION`). The `source` field is prefixed by
//! kind (`git+…` / `url+…` / `path+…`) so a future `registry+…` slots in without
//! a schema change. A PATH entry records NO `integrity` (local + mutable — the
//! explicitly non-reproducible escape hatch).

use std::path::Path;

/// The current `ascript.lock` format version (the lock's OWN counter).
pub const LOCK_VERSION: u64 = 1;

/// A parsed `ascript.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    /// Lockfile format version (own counter, starts at 1).
    pub version: u64,
    /// Resolved packages, kept SORTED by `name` (enforced on write).
    pub packages: Vec<LockEntry>,
}

/// One resolved package in the lockfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// The package key (first-segment / `@scope/name`).
    pub name: String,
    /// Prefixed source: `git+<url>` / `url+<url>` / `path+<rel>`.
    pub source: String,
    /// What the manifest asked for (a git TAG), if any — for `tree`/`update`.
    pub requirement: Option<String>,
    /// The version MVS selected: the tag/rev for git, the fetched
    /// `[package].version` for url, or the path string for a path dep.
    pub resolved: String,
    /// The exact git commit a tag/rev resolved to at lock time. `None` for
    /// url/path deps.
    pub rev: Option<String>,
    /// `asum1-…` integrity over the normalized tree. `None` for a PATH dep
    /// (local + mutable, unhashed by design).
    pub integrity: Option<String>,
}

impl Lockfile {
    /// Serialize to canonical TOML: `version = N` then `[[package]]` blocks
    /// sorted by name, fields in a fixed order, optional fields omitted when
    /// absent. Byte-stable (write→read→write is identical).
    pub fn to_toml(&self) -> String {
        let mut entries = self.packages.clone();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        let mut out = String::new();
        out.push_str(&format!("version = {}\n", self.version));
        for e in &entries {
            out.push_str("\n[[package]]\n");
            out.push_str(&format!("name = {}\n", toml_str(&e.name)));
            out.push_str(&format!("source = {}\n", toml_str(&e.source)));
            if let Some(req) = &e.requirement {
                out.push_str(&format!("requirement = {}\n", toml_str(req)));
            }
            out.push_str(&format!("resolved = {}\n", toml_str(&e.resolved)));
            if let Some(rev) = &e.rev {
                out.push_str(&format!("rev = {}\n", toml_str(rev)));
            }
            if let Some(integrity) = &e.integrity {
                out.push_str(&format!("integrity = {}\n", toml_str(integrity)));
            }
        }
        out
    }

    /// Parse from `ascript.lock` text. `ctx` is used only in error messages.
    pub fn parse(ctx: &str, text: &str) -> Result<Lockfile, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e| format!("ascript.lock: {ctx}: malformed TOML: {e}"))?;

        let version = match table.get("version") {
            Some(toml::Value::Integer(n)) if *n >= 0 => *n as u64,
            Some(_) => return Err(format!("ascript.lock: {ctx}: `version` must be an integer")),
            None => return Err(format!("ascript.lock: {ctx}: missing `version`")),
        };

        let mut packages = Vec::new();
        if let Some(arr) = table.get("package") {
            let arr = arr.as_array().ok_or_else(|| {
                format!("ascript.lock: {ctx}: `package` must be an array of tables")
            })?;
            for item in arr {
                let t = item.as_table().ok_or_else(|| {
                    format!("ascript.lock: {ctx}: each `[[package]]` must be a table")
                })?;
                packages.push(parse_entry(ctx, t)?);
            }
        }
        // Keep canonical (sorted) order on load too.
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Lockfile { version, packages })
    }

    /// Read the lockfile beside `manifest_dir` (`<manifest_dir>/ascript.lock`),
    /// if present. `Ok(None)` when there is no lockfile.
    pub fn read_beside(manifest_dir: &Path) -> Result<Option<Lockfile>, String> {
        let path = manifest_dir.join("ascript.lock");
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("ascript.lock: {}: {e}", path.display()))?;
        Lockfile::parse(&path.display().to_string(), &text).map(Some)
    }

    /// Write the lockfile beside `manifest_dir` (`<manifest_dir>/ascript.lock`).
    ///
    /// Atomic: written to a pid-qualified sibling temp, then `rename`d over the
    /// target (POSIX rename is atomic at the directory level). The lockfile is read
    /// back as a source of truth by `--locked`, so a crash mid-write must not leave a
    /// partial/empty `ascript.lock` that fails to parse — the rename guarantees the
    /// file holds either the previous or the new complete lockfile, never a torn one.
    pub fn write_beside(&self, manifest_dir: &Path) -> Result<(), String> {
        let path = manifest_dir.join("ascript.lock");
        let tmp = path.with_extension(format!("lock.{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.to_toml())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", path.display())
        })
    }
}

fn parse_entry(ctx: &str, t: &toml::Table) -> Result<LockEntry, String> {
    let req_str = |key: &str| -> Result<String, String> {
        match t.get(key) {
            Some(toml::Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(format!("ascript.lock: {ctx}: `{key}` must be a string")),
            None => Err(format!("ascript.lock: {ctx}: `[[package]]` missing `{key}`")),
        }
    };
    let opt_str = |key: &str| -> Result<Option<String>, String> {
        match t.get(key) {
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("ascript.lock: {ctx}: `{key}` must be a string")),
            None => Ok(None),
        }
    };
    Ok(LockEntry {
        name: req_str("name")?,
        source: req_str("source")?,
        requirement: opt_str("requirement")?,
        resolved: req_str("resolved")?,
        rev: opt_str("rev")?,
        integrity: opt_str("integrity")?,
    })
}

/// Render a string as a TOML basic string (escape `"` / `\` / control chars). The
/// inputs (names, urls, base64url hashes) are simple, but escaping keeps it safe.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lockfile {
        Lockfile {
            version: 1,
            packages: vec![
                LockEntry {
                    name: "http".into(),
                    source: "git+https://example.com/as-http".into(),
                    requirement: Some("v1.4.0".into()),
                    resolved: "v1.4.0".into(),
                    rev: Some("9f3ce21".into()),
                    integrity: Some("asum1-abc".into()),
                },
                LockEntry {
                    name: "util".into(),
                    source: "path+../util".into(),
                    requirement: None,
                    resolved: "../util".into(),
                    rev: None,
                    integrity: None, // path dep: no integrity
                },
            ],
        }
    }

    #[test]
    fn round_trip_byte_stable() {
        let lock = sample();
        let s1 = lock.to_toml();
        let parsed = Lockfile::parse("ascript.lock", &s1).unwrap();
        let s2 = parsed.to_toml();
        assert_eq!(s1, s2, "write->read->write must be byte-stable");
        assert_eq!(lock, parsed);
    }

    #[test]
    fn entries_sorted_by_name() {
        let lock = Lockfile {
            version: 1,
            packages: vec![
                LockEntry {
                    name: "zeta".into(),
                    source: "path+../z".into(),
                    requirement: None,
                    resolved: "../z".into(),
                    rev: None,
                    integrity: None,
                },
                LockEntry {
                    name: "alpha".into(),
                    source: "path+../a".into(),
                    requirement: None,
                    resolved: "../a".into(),
                    rev: None,
                    integrity: None,
                },
            ],
        };
        let s = lock.to_toml();
        let alpha_pos = s.find("alpha").unwrap();
        let zeta_pos = s.find("zeta").unwrap();
        assert!(alpha_pos < zeta_pos, "alpha must sort before zeta:\n{s}");
    }

    #[test]
    fn path_entry_omits_integrity() {
        let lock = sample();
        let s = lock.to_toml();
        // The util (path) block has no integrity line; the http block does.
        let util_block = s.split("[[package]]").find(|b| b.contains("util")).unwrap();
        assert!(!util_block.contains("integrity"), "path dep must omit integrity");
        let http_block = s.split("[[package]]").find(|b| b.contains("http")).unwrap();
        assert!(http_block.contains("integrity = \"asum1-abc\""));
        assert!(http_block.contains("rev = \"9f3ce21\""));
        assert!(http_block.contains("requirement = \"v1.4.0\""));
    }

    #[test]
    fn version_header_present() {
        assert!(sample().to_toml().starts_with("version = 1\n"));
    }

    #[test]
    fn empty_round_trips() {
        let lock = Lockfile {
            version: LOCK_VERSION,
            packages: Vec::new(),
        };
        let s = lock.to_toml();
        let parsed = Lockfile::parse("ascript.lock", &s).unwrap();
        assert_eq!(lock, parsed);
        assert!(parsed.packages.is_empty());
    }

    #[test]
    fn missing_version_is_error() {
        let e = Lockfile::parse("ascript.lock", "[[package]]\nname=\"x\"\n").unwrap_err();
        assert!(e.contains("missing `version`"), "{e}");
    }

    #[test]
    fn write_read_beside_tempdir() {
        let dir = std::env::temp_dir().join(format!("lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = sample();
        lock.write_beside(&dir).unwrap();
        let back = Lockfile::read_beside(&dir).unwrap().unwrap();
        assert_eq!(lock, back);
        // No lockfile in a fresh dir → None.
        let empty_dir = std::env::temp_dir().join(format!("lock-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty_dir).unwrap();
        assert!(Lockfile::read_beside(&empty_dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty_dir);
    }
}
