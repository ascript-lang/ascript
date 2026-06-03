//! `ascript.toml` `[lint]` config discovery + parsing for the `ascript check` CLI.
//!
//! This lives in the CLI binary (NOT `src/check/`, which stays toml-free and
//! feature-independent). It uses the `toml` crate (a non-optional dependency, so
//! it is available under `--no-default-features` too) to parse a `[lint]` table
//! into severity overrides that seed a [`ascript::check::LintConfig`]. CLI flags
//! are then overlaid on top, so the net precedence is:
//!
//!   inline `// ascript-ignore[code]` (suppression, in `analyze.rs`)
//!     > CLI `--deny`/`--warn`/`--allow`/`--deny-warnings`
//!     > `ascript.toml [lint]`
//!     > rule default
//!
//! Discovery walks UP from the checked file's parent directory (falling back to
//! the current dir for a bare/relative file) to the filesystem root, project-root
//! marker style; the first `ascript.toml` found wins. Discovery is per-file so a
//! file is governed by the config nearest to it.

use ascript::check::LintConfig;
use std::path::{Path, PathBuf};

/// A parsed `[lint]` table from an `ascript.toml`.
#[derive(Debug, Default)]
pub struct TomlLint {
    pub deny: Vec<String>,
    pub warn: Vec<String>,
    pub allow: Vec<String>,
    pub deny_warnings: bool,
}

/// Find the nearest `ascript.toml` at or above `file`'s directory.
///
/// Walks up from the file's parent directory to the filesystem root; the first
/// `ascript.toml` found wins. A relative/bare file path anchors the walk at the
/// current working directory. Returns `None` if no `ascript.toml` exists in the
/// chain (callers then fall back to the default config — unchanged behavior).
pub fn discover(file: &Path) -> Option<PathBuf> {
    // Anchor on the file's parent dir; for a bare filename use the cwd so we still
    // pick up an `ascript.toml` sitting next to it.
    let start = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::env::current_dir().ok()?,
    };
    let mut dir: &Path = &start;
    loop {
        let candidate = dir.join("ascript.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Parse the `[lint]` table out of an `ascript.toml`'s text.
///
/// Returns a clear, file-named error string on malformed toml or a `[lint]`
/// field of the wrong type. A missing `[lint]` table (or a missing file) yields
/// an empty `TomlLint` — i.e. no overrides.
pub fn parse_lint(path: &Path, text: &str) -> Result<TomlLint, String> {
    let name = path.display();
    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("ascript.toml: {name}: malformed TOML: {e}"))?;

    let mut out = TomlLint::default();
    let lint = match table.get("lint") {
        None => return Ok(out), // no [lint] table → no config
        Some(toml::Value::Table(t)) => t,
        Some(_) => {
            return Err(format!(
                "ascript.toml: {name}: `lint` must be a table (`[lint]`)"
            ));
        }
    };

    for (field, dest) in [
        ("deny", &mut out.deny),
        ("warn", &mut out.warn),
        ("allow", &mut out.allow),
    ] {
        if let Some(v) = lint.get(field) {
            let arr = v.as_array().ok_or_else(|| {
                format!("ascript.toml: {name}: `lint.{field}` must be an array of strings")
            })?;
            for item in arr {
                let s = item.as_str().ok_or_else(|| {
                    format!("ascript.toml: {name}: `lint.{field}` must be an array of strings")
                })?;
                dest.push(s.to_string());
            }
        }
    }

    if let Some(v) = lint.get("deny_warnings") {
        out.deny_warnings = v.as_bool().ok_or_else(|| {
            format!("ascript.toml: {name}: `lint.deny_warnings` must be a boolean")
        })?;
    }

    Ok(out)
}

/// Apply a parsed `[lint]` table onto `config`, validating every rule code
/// against the known set. An unknown code → a clear, file-named error.
///
/// `deny_warnings` is additive: it can only turn the flag ON (CLI may also turn
/// it on; neither can turn the other off).
pub fn apply(path: &Path, lint: &TomlLint, config: &mut LintConfig) -> Result<(), String> {
    let name = path.display();
    for (kind, codes) in [
        ("deny", &lint.deny),
        ("warn", &lint.warn),
        ("allow", &lint.allow),
    ] {
        for code in codes {
            if !LintConfig::is_known_code(code) {
                return Err(format!(
                    "ascript.toml: {name}: unknown lint rule '{code}' in `lint.{kind}` (known rules: {})",
                    ascript::check::RULE_CODES.join(", ")
                ));
            }
            match kind {
                "deny" => config.deny(code),
                "warn" => config.warn(code),
                _ => config.allow(code),
            }
        }
    }
    if lint.deny_warnings {
        config.deny_warnings = true;
    }
    Ok(())
}

/// Build a `LintConfig` seeded from `file`'s nearest `ascript.toml [lint]` table.
///
/// On any toml problem (malformed, wrong field type, unknown rule) returns a
/// clear error string naming `ascript.toml`. No `ascript.toml` → an unchanged
/// default config (preserving prior behavior).
pub fn config_for_file(file: &Path) -> Result<LintConfig, String> {
    let mut config = LintConfig::default();
    if let Some(path) = discover(file) {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("ascript.toml: {}: {e}", path.display()))?;
        let lint = parse_lint(&path, &text)?;
        apply(&path, &lint, &mut config)?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extracts_lists_and_flag() {
        let toml = "[lint]\ndeny = [\"unused-binding\"]\nwarn = [\"shadowing\"]\nallow = [\"unused-import\"]\ndeny_warnings = true\n";
        let l = parse_lint(Path::new("ascript.toml"), toml).unwrap();
        assert_eq!(l.deny, ["unused-binding"]);
        assert_eq!(l.warn, ["shadowing"]);
        assert_eq!(l.allow, ["unused-import"]);
        assert!(l.deny_warnings);
    }

    #[test]
    fn no_lint_table_is_empty() {
        let l = parse_lint(Path::new("ascript.toml"), "[other]\nx = 1\n").unwrap();
        assert!(l.deny.is_empty() && !l.deny_warnings);
    }

    #[test]
    fn wrong_field_type_is_error_naming_file() {
        let e = parse_lint(Path::new("ascript.toml"), "[lint]\ndeny = \"nope\"\n").unwrap_err();
        assert!(e.contains("ascript.toml") && e.contains("deny"), "{e}");
    }

    #[test]
    fn malformed_toml_is_error_naming_file() {
        let e = parse_lint(Path::new("ascript.toml"), "[lint\n").unwrap_err();
        assert!(e.contains("ascript.toml") && e.contains("malformed"), "{e}");
    }

    #[test]
    fn apply_rejects_unknown_rule() {
        let mut cfg = LintConfig::default();
        let lint = TomlLint {
            deny: vec!["bogus".to_string()],
            ..Default::default()
        };
        let e = apply(Path::new("ascript.toml"), &lint, &mut cfg).unwrap_err();
        assert!(e.contains("ascript.toml") && e.contains("bogus"), "{e}");
    }

    #[test]
    fn apply_seeds_overrides() {
        let mut cfg = LintConfig::default();
        let lint = TomlLint {
            allow: vec!["unused-binding".to_string()],
            deny_warnings: true,
            ..Default::default()
        };
        apply(Path::new("ascript.toml"), &lint, &mut cfg).unwrap();
        assert_eq!(
            cfg.effective("unused-binding", ascript::check::Severity::Warning),
            None
        );
        assert!(cfg.deny_warnings);
    }
}
