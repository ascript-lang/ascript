//! Parse the `ascript.toml` `[package]` + `[dependencies]` tables (SP6 §2).
//!
//! This follows the same model as `ascript::check::config_toml` and
//! same model: parse a `toml::Table`, read only the tables we own (`[package]` /
//! `[dependencies]` — `[lint]` is left to the lint parser), and emit clear,
//! file-named errors (`ascript.toml: <ctx>: …`). The package parser is orthogonal
//! to the lint parser: each reads only its own keys, so neither errors on the
//! other's tables.
//!
//! The dependency-value SHAPE selects the source kind (decentralized-first, D1):
//! `{ git, tag|rev }`, `{ url }`, `{ path }`, or a bare-version STRING — the last
//! being the reserved-future registry requirement, which SP6 parses but rejects at
//! resolve time with a clear "needs a registry" error.

use indexmap::IndexMap;
use std::path::{Path, PathBuf};

/// A parsed `ascript.toml` manifest: the optional `[package]` identity table plus
/// the `[dependencies]` map.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// `[package]` — optional for a leaf application (it can declare deps without
    /// being publishable). Required (with `name`+`version`+resolvable `entry`) for
    /// a directory to be usable AS a dependency.
    pub package: Option<PackageMeta>,
    /// `[dependencies]` — name → source. Iterated in the `toml` crate's key
    /// order (alphabetical, since `preserve_order` is off); MVS is
    /// order-independent and the lockfile sorts by name, so order is immaterial.
    pub dependencies: IndexMap<String, DepSource>,
    /// `[capabilities]` — FFI §4.2 opt-out capability denials (a NEW owned table,
    /// beside `[package]`/`[dependencies]`). `None` when the table is absent (the
    /// default: all granted). Composed into a `CapSet` via [`Manifest::capset`].
    pub capabilities: Option<CapabilitiesConfig>,
}

/// The parsed `[capabilities]` table (FFI §4.2). Pure DATA — the composition into
/// a runtime `CapSet` happens in [`Manifest::capset`], keeping the TOML shape and
/// the runtime type loosely coupled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitiesConfig {
    /// `deny = ["ffi", "process"]` — whole-capability denials.
    pub deny: Vec<String>,
    /// `net = { deny = "external", allow = [...] }` — a granular net carve-out.
    pub net: Option<ScopeConfig>,
    /// `fs = { deny = "write", allow = [...] }` — a granular fs carve-out.
    pub fs: Option<ScopeConfig>,
}

/// A granular `{ deny = "<mode>", allow = [...] }` carve-out, as written in TOML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeConfig {
    /// The deny mode string: for net `"external"`/`"all"`, for fs `"write"`/`"all"`.
    pub deny: String,
    /// The allow-list (hosts for net, path prefixes for fs).
    pub allow: Vec<String>,
}

/// The `[package]` identity table.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageMeta {
    /// `name` — `^(@[a-z0-9-]+/)?[a-z0-9][a-z0-9-]*$`. Optional in the file (a leaf
    /// app may omit `[package]` entirely), but if `[package]` is present `name` is
    /// required.
    pub name: String,
    /// `version` — strict `MAJOR.MINOR.PATCH`. Required when `[package]` is present.
    pub version: Version,
    /// `entry` — the module a dependent binds for a bare `import "<name>"`.
    /// Defaults are tried at LOAD time (`src/main.as` → `main.as` → `<name>.as`);
    /// `None` here means "no explicit entry, use the defaults".
    pub entry: Option<String>,
    /// Optional metadata.
    pub description: Option<String>,
    /// Optional metadata.
    pub license: Option<String>,
}

/// A strict `MAJOR.MINOR.PATCH` semantic version (the MVS comparison unit, §3).
/// Pre-release / build metadata are out of scope for SP6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse a strict `MAJOR.MINOR.PATCH` triple. Accepts an optional single
    /// leading `v` (git tags are conventionally `vX.Y.Z`); rejects ranges,
    /// pre-release, build metadata, or a missing component.
    pub fn parse(s: &str) -> Result<Version, String> {
        let core = s.strip_prefix('v').unwrap_or(s);
        let mut it = core.split('.');
        let major = parse_component(it.next())?;
        let minor = parse_component(it.next())?;
        let patch = parse_component(it.next())?;
        if it.next().is_some() {
            return Err(format!("expected MAJOR.MINOR.PATCH, found '{s}'"));
        }
        Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

fn parse_component(c: Option<&str>) -> Result<u64, String> {
    let c = c.ok_or("expected MAJOR.MINOR.PATCH")?;
    if c.is_empty() || !c.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid version component '{c}'"));
    }
    c.parse::<u64>().map_err(|e| e.to_string())
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Which git ref a `{ git, … }` dependency pins to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitPin {
    /// `{ git, tag = "v1.4.0" }` — the tag IS the declared version (MVS-comparable).
    Tag(String),
    /// `{ git, rev = "a1b2c3d" }` — an exact commit; a non-versioned leaf in MVS.
    Rev(String),
}

/// A single `[dependencies]` value: its SHAPE selects the source kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepSource {
    /// `{ git = "…", tag|rev = "…" }`.
    Git { url: String, pin: GitPin },
    /// `{ url = "…" }` — a tarball/zip URL.
    Url { url: String },
    /// `{ path = "…" }` — a local directory, relative to the manifest dir.
    Path { path: String },
    /// A bare-version requirement STRING (`"^1.2.0"`) — the reserved-future
    /// registry source kind. SP6 parses it but the resolver rejects it with a
    /// clear "needs a registry" error (§10). The string is kept verbatim.
    Registry { req: String },
}

/// Validate a package `name` against `^(@[a-z0-9-]+/)?[a-z0-9][a-z0-9-]*$`.
pub fn is_valid_name(name: &str) -> bool {
    let body = match name.strip_prefix('@') {
        Some(rest) => {
            // Scoped: `@scope/name`. Exactly one `/`, scope is `[a-z0-9-]+`.
            let Some((scope, pkg)) = rest.split_once('/') else {
                return false;
            };
            if scope.is_empty()
                || !scope
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return false;
            }
            pkg
        }
        None => name,
    };
    // Unscoped name body: `[a-z0-9][a-z0-9-]*`.
    let mut bytes = body.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Manifest {
    /// Parse a manifest from `ascript.toml` text. `ctx` is used only in error
    /// messages (typically the file path display).
    pub fn parse(ctx: &str, text: &str) -> Result<Manifest, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e| format!("ascript.toml: {ctx}: malformed TOML: {e}"))?;

        let package = match table.get("package") {
            None => None,
            Some(toml::Value::Table(t)) => Some(parse_package(ctx, t)?),
            Some(_) => {
                return Err(format!(
                    "ascript.toml: {ctx}: `package` must be a table (`[package]`)"
                ))
            }
        };

        let mut dependencies = IndexMap::new();
        match table.get("dependencies") {
            None => {}
            Some(toml::Value::Table(t)) => {
                for (name, val) in t {
                    let src = parse_dep_source(ctx, name, val)?;
                    dependencies.insert(name.clone(), src);
                }
            }
            Some(_) => {
                return Err(format!(
                    "ascript.toml: {ctx}: `dependencies` must be a table (`[dependencies]`)"
                ))
            }
        }

        let capabilities = match table.get("capabilities") {
            None => None,
            Some(toml::Value::Table(t)) => Some(parse_capabilities(ctx, t)?),
            Some(_) => {
                return Err(format!(
                    "ascript.toml: {ctx}: `capabilities` must be a table (`[capabilities]`)"
                ))
            }
        };

        Ok(Manifest {
            package,
            dependencies,
            capabilities,
        })
    }

    /// Compose the manifest's `[capabilities]` into a runtime [`CapSet`]
    /// (FFI §4.2/§4.5). `None` capabilities → the default all-granted set. An
    /// unknown cap name or deny-mode string is a clean `Err` (never a panic — a
    /// hostile manifest is rejected, not crashed).
    pub fn capset(&self) -> Result<ascript::stdlib::caps::CapSet, String> {
        use ascript::stdlib::caps::{Cap, CapSet, FsDeny, FsScope, NetDeny, NetScope};
        let mut set = CapSet::all_granted();
        let Some(caps) = &self.capabilities else {
            return Ok(set);
        };
        // Whole-capability denials.
        for name in &caps.deny {
            match ascript::stdlib::caps::cap_name(name) {
                Some(cap) => set.deny(cap),
                None => {
                    return Err(format!(
                        "ascript.toml: [capabilities].deny: unknown capability '{name}' \
                         (expected one of: fs, net, process, ffi, env)"
                    ))
                }
            }
        }
        // Granular net carve-out.
        if let Some(sc) = &caps.net {
            let deny = match sc.deny.as_str() {
                "external" => NetDeny::External,
                "all" => NetDeny::All,
                other => {
                    return Err(format!(
                        "ascript.toml: [capabilities].net.deny: expected \"external\" or \"all\", got '{other}'"
                    ))
                }
            };
            set.set_net_scope(NetScope {
                deny,
                allow: sc.allow.clone(),
            });
        }
        // Granular fs carve-out.
        if let Some(sc) = &caps.fs {
            let deny = match sc.deny.as_str() {
                "write" => FsDeny::Write,
                "all" => FsDeny::All,
                other => {
                    return Err(format!(
                        "ascript.toml: [capabilities].fs.deny: expected \"write\" or \"all\", got '{other}'"
                    ))
                }
            };
            set.set_fs_scope(FsScope {
                deny,
                allow: sc.allow.clone(),
            });
        }
        // A whole-cap deny of fs/net overrides any carve-out (deny is monotone):
        // re-apply after scopes so `deny = ["net"]` + a net carve-out → fully denied.
        for name in &caps.deny {
            if let Some(cap) = ascript::stdlib::caps::cap_name(name) {
                if matches!(cap, Cap::Fs | Cap::Net) {
                    set.deny(cap);
                }
            }
        }
        Ok(set)
    }

    /// Load the nearest `ascript.toml` at or above `file`'s directory, returning
    /// its directory (the project root) and the parsed manifest. Reuses the
    /// lint-config upward-walk discovery so a file is governed by the config
    /// nearest to it. `Ok(None)` means no `ascript.toml` was found in the chain.
    pub fn load_nearest(file: &Path) -> Result<Option<(PathBuf, Manifest)>, String> {
        let Some(path) = ascript::check::config_toml::discover(file) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("ascript.toml: {}: {e}", path.display()))?;
        let manifest = Manifest::parse(&path.display().to_string(), &text)?;
        let root = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(Some((root, manifest)))
    }
}

fn parse_package(ctx: &str, t: &toml::Table) -> Result<PackageMeta, String> {
    let str_field = |key: &str| -> Result<Option<String>, String> {
        match t.get(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!(
                "ascript.toml: {ctx}: `package.{key}` must be a string"
            )),
        }
    };

    let name = str_field("name")?
        .ok_or_else(|| format!("ascript.toml: {ctx}: `[package]` requires a `name`"))?;
    if !is_valid_name(&name) {
        return Err(format!(
            "ascript.toml: {ctx}: invalid package name '{name}' (must match \
             `^(@scope/)?[a-z0-9][a-z0-9-]*$`)"
        ));
    }

    let version_str = str_field("version")?
        .ok_or_else(|| format!("ascript.toml: {ctx}: `[package]` requires a `version`"))?;
    let version = Version::parse(&version_str)
        .map_err(|e| format!("ascript.toml: {ctx}: `package.version`: {e}"))?;

    Ok(PackageMeta {
        name,
        version,
        entry: str_field("entry")?,
        description: str_field("description")?,
        license: str_field("license")?,
    })
}

/// Parse the `[capabilities]` table (FFI §4.2). Reads `deny = [..]` plus optional
/// granular `net`/`fs` carve-out sub-tables. Unknown KEYS are ignored (forward
/// compatibility), but a malformed VALUE (wrong type) is a clean error.
fn parse_capabilities(ctx: &str, t: &toml::Table) -> Result<CapabilitiesConfig, String> {
    let mut cfg = CapabilitiesConfig::default();
    if let Some(v) = t.get("deny") {
        cfg.deny = parse_string_array(ctx, "capabilities.deny", v)?;
    }
    if let Some(v) = t.get("net") {
        cfg.net = Some(parse_scope(ctx, "capabilities.net", v)?);
    }
    if let Some(v) = t.get("fs") {
        cfg.fs = Some(parse_scope(ctx, "capabilities.fs", v)?);
    }
    Ok(cfg)
}

/// Parse a `{ deny = "<mode>", allow = [..] }` carve-out sub-table.
fn parse_scope(ctx: &str, key: &str, val: &toml::Value) -> Result<ScopeConfig, String> {
    let toml::Value::Table(t) = val else {
        return Err(format!(
            "ascript.toml: {ctx}: `{key}` must be a table ({{ deny = \"…\", allow = [..] }})"
        ));
    };
    let deny = match t.get("deny") {
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => return Err(format!("ascript.toml: {ctx}: `{key}.deny` must be a string")),
        None => return Err(format!("ascript.toml: {ctx}: `{key}` requires a `deny` mode")),
    };
    let allow = match t.get("allow") {
        None => Vec::new(),
        Some(v) => parse_string_array(ctx, &format!("{key}.allow"), v)?,
    };
    Ok(ScopeConfig { deny, allow })
}

/// Parse a `key = ["a", "b"]` array-of-strings, erroring on a non-array or a
/// non-string element.
fn parse_string_array(ctx: &str, key: &str, val: &toml::Value) -> Result<Vec<String>, String> {
    let toml::Value::Array(arr) = val else {
        return Err(format!(
            "ascript.toml: {ctx}: `{key}` must be an array of strings"
        ));
    };
    let mut out = Vec::with_capacity(arr.len());
    for el in arr {
        match el {
            toml::Value::String(s) => out.push(s.clone()),
            _ => {
                return Err(format!(
                    "ascript.toml: {ctx}: `{key}` must contain only strings"
                ))
            }
        }
    }
    Ok(out)
}

fn parse_dep_source(ctx: &str, name: &str, val: &toml::Value) -> Result<DepSource, String> {
    match val {
        // A bare string is a reserved-future registry requirement.
        toml::Value::String(s) => Ok(DepSource::Registry { req: s.clone() }),
        toml::Value::Table(t) => parse_dep_table(ctx, name, t),
        _ => Err(format!(
            "ascript.toml: {ctx}: dependency '{name}' must be a version string or an \
             inline table ({{ git=… }}, {{ url=… }}, or {{ path=… }})"
        )),
    }
}

fn parse_dep_table(ctx: &str, name: &str, t: &toml::Table) -> Result<DepSource, String> {
    let has_git = t.contains_key("git");
    let has_url = t.contains_key("url");
    let has_path = t.contains_key("path");

    // Exactly one source key must be present.
    let kinds = [has_git, has_url, has_path].iter().filter(|b| **b).count();
    if kinds == 0 {
        return Err(format!(
            "ascript.toml: {ctx}: dependency '{name}' must specify one of `git`, `url`, or `path`"
        ));
    }
    if kinds > 1 {
        return Err(format!(
            "ascript.toml: {ctx}: dependency '{name}' mixes source keys \
             (use exactly one of `git`, `url`, or `path`)"
        ));
    }

    let get_str = |key: &str| -> Result<String, String> {
        match t.get(key) {
            Some(toml::Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(format!(
                "ascript.toml: {ctx}: dependency '{name}': `{key}` must be a string"
            )),
            None => unreachable!("caller checked the key is present"),
        }
    };

    if has_git {
        let url = get_str("git")?;
        let has_tag = t.contains_key("tag");
        let has_rev = t.contains_key("rev");
        let pin = match (has_tag, has_rev) {
            (true, false) => GitPin::Tag(get_str("tag")?),
            (false, true) => GitPin::Rev(get_str("rev")?),
            (false, false) => {
                return Err(format!(
                    "ascript.toml: {ctx}: git dependency '{name}' requires exactly one of \
                     `tag` or `rev`"
                ))
            }
            (true, true) => {
                return Err(format!(
                    "ascript.toml: {ctx}: git dependency '{name}' specifies both `tag` and \
                     `rev` (use exactly one)"
                ))
            }
        };
        return Ok(DepSource::Git { url, pin });
    }

    if has_url {
        // Reject stray `tag`/`rev`/`path` mixed with `url`.
        for stray in ["tag", "rev", "path"] {
            if t.contains_key(stray) {
                return Err(format!(
                    "ascript.toml: {ctx}: url dependency '{name}' has an unexpected `{stray}` key"
                ));
            }
        }
        return Ok(DepSource::Url {
            url: get_str("url")?,
        });
    }

    // has_path
    for stray in ["tag", "rev"] {
        if t.contains_key(stray) {
            return Err(format!(
                "ascript.toml: {ctx}: path dependency '{name}' has an unexpected `{stray}` key"
            ));
        }
    }
    Ok(DepSource::Path {
        path: get_str("path")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_and_all_dep_kinds() {
        let src = r#"
[package]
name = "myapp"
version = "0.3.1"
entry = "src/main.as"
description = "demo"
license = "MIT"

[dependencies]
http   = { git = "https://example.com/as-http", tag = "v1.4.0" }
schema = { git = "https://example.com/as-schema", rev = "a1b2c3d" }
parse  = { url = "https://example.com/as-parse-1.2.0.tar.gz" }
util   = { path = "../util" }
color  = "^1.2.0"
"#;
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let pkg = m.package.unwrap();
        assert_eq!(pkg.name, "myapp");
        assert_eq!(
            pkg.version,
            Version {
                major: 0,
                minor: 3,
                patch: 1
            }
        );
        assert_eq!(pkg.entry.as_deref(), Some("src/main.as"));
        assert_eq!(pkg.description.as_deref(), Some("demo"));
        assert_eq!(pkg.license.as_deref(), Some("MIT"));

        assert_eq!(
            m.dependencies["http"],
            DepSource::Git {
                url: "https://example.com/as-http".to_string(),
                pin: GitPin::Tag("v1.4.0".to_string())
            }
        );
        assert_eq!(
            m.dependencies["schema"],
            DepSource::Git {
                url: "https://example.com/as-schema".to_string(),
                pin: GitPin::Rev("a1b2c3d".to_string())
            }
        );
        assert_eq!(
            m.dependencies["parse"],
            DepSource::Url {
                url: "https://example.com/as-parse-1.2.0.tar.gz".to_string()
            }
        );
        assert_eq!(
            m.dependencies["util"],
            DepSource::Path {
                path: "../util".to_string()
            }
        );
        assert_eq!(
            m.dependencies["color"],
            DepSource::Registry {
                req: "^1.2.0".to_string()
            }
        );
    }

    #[test]
    fn all_deps_captured() {
        // The `toml` crate's `Table` is a sorted map (the `preserve_order`
        // feature is off), so iteration is alphabetical — fine, since MVS is
        // order-independent and the lockfile sorts by name. Assert the SET.
        let src = r#"
[dependencies]
zeta = { path = "../z" }
alpha = { path = "../a" }
"#;
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let mut keys: Vec<&String> = m.dependencies.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["alpha", "zeta"]);
    }

    #[test]
    fn package_optional_for_leaf_app() {
        let m = Manifest::parse("ascript.toml", "[dependencies]\nu = { path = \"../u\" }\n").unwrap();
        assert!(m.package.is_none());
        assert_eq!(m.dependencies.len(), 1);
    }

    #[test]
    fn empty_manifest_ok() {
        let m = Manifest::parse("ascript.toml", "").unwrap();
        assert!(m.package.is_none());
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn capabilities_table_parses_into_capset() {
        use ascript::stdlib::caps::Cap;
        let src = r#"
[capabilities]
deny = ["ffi", "process"]
"#;
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let cfg = m.capabilities.as_ref().unwrap();
        assert_eq!(cfg.deny, vec!["ffi", "process"]);
        let cs = m.capset().unwrap();
        assert!(!cs.has(Cap::Ffi));
        assert!(!cs.has(Cap::Process));
        assert!(cs.has(Cap::Fs) && cs.has(Cap::Net) && cs.has(Cap::Env));
    }

    #[test]
    fn capabilities_granular_net_and_fs_parse() {
        use ascript::stdlib::caps::{Cap, NetDeny};
        let src = r#"
[capabilities]
net = { deny = "external", allow = ["api.internal"] }
fs  = { deny = "write", allow = ["./cache"] }
"#;
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let cs = m.capset().unwrap();
        // The carve-out clears the outright bit but configures the scope.
        assert!(!cs.has(Cap::Net));
        let net = cs.net_scope.as_ref().unwrap();
        assert_eq!(net.deny, NetDeny::External);
        assert_eq!(net.allow, vec!["api.internal".to_string()]);
        let fs = cs.fs_scope.as_ref().unwrap();
        assert_eq!(fs.allow, vec!["./cache".to_string()]);
    }

    #[test]
    fn capabilities_unknown_cap_name_is_clean_error() {
        let src = "[capabilities]\ndeny = [\"bogus\"]\n";
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let e = m.capset().unwrap_err();
        assert!(e.contains("unknown capability") && e.contains("bogus"), "{e}");
    }

    #[test]
    fn capabilities_bad_deny_mode_is_clean_error() {
        let src = "[capabilities]\nnet = { deny = \"weird\" }\n";
        let m = Manifest::parse("ascript.toml", src).unwrap();
        let e = m.capset().unwrap_err();
        assert!(e.contains("external") && e.contains("weird"), "{e}");
    }

    #[test]
    fn capabilities_absent_is_all_granted() {
        use ascript::stdlib::caps::Cap;
        let m = Manifest::parse("ascript.toml", "[dependencies]\n").unwrap();
        assert!(m.capabilities.is_none());
        let cs = m.capset().unwrap();
        for cap in Cap::ALL {
            assert!(cs.has(cap));
        }
    }

    #[test]
    fn capabilities_non_table_is_error() {
        let e = Manifest::parse("ascript.toml", "capabilities = 5\n").unwrap_err();
        assert!(e.contains("capabilities") && e.contains("must be a table"), "{e}");
    }

    #[test]
    fn capabilities_deny_must_be_string_array() {
        let e = Manifest::parse("ascript.toml", "[capabilities]\ndeny = \"ffi\"\n").unwrap_err();
        assert!(e.contains("must be an array of strings"), "{e}");
    }

    #[test]
    fn lint_table_is_inert() {
        // A `[lint]` table is ignored by the package parser (orthogonality).
        let m = Manifest::parse("ascript.toml", "[lint]\ndeny = [\"x\"]\n").unwrap();
        assert!(m.package.is_none());
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn mixed_git_and_path_is_error() {
        let src = "[dependencies]\nbad = { git = \"u\", path = \"../p\", tag = \"v1.0.0\" }\n";
        let e = Manifest::parse("ascript.toml", src).unwrap_err();
        assert!(e.contains("ascript.toml") && e.contains("bad") && e.contains("mixes"), "{e}");
    }

    #[test]
    fn git_requires_tag_or_rev() {
        let e = Manifest::parse("ascript.toml", "[dependencies]\ng = { git = \"u\" }\n")
            .unwrap_err();
        assert!(e.contains("requires exactly one of") && e.contains("tag"), "{e}");
    }

    #[test]
    fn git_tag_and_rev_both_is_error() {
        let src = "[dependencies]\ng = { git = \"u\", tag = \"v1.0.0\", rev = \"abc\" }\n";
        let e = Manifest::parse("ascript.toml", src).unwrap_err();
        assert!(e.contains("both") && e.contains("tag"), "{e}");
    }

    #[test]
    fn unknown_source_kind_is_error() {
        let e = Manifest::parse("ascript.toml", "[dependencies]\nx = { weird = 1 }\n")
            .unwrap_err();
        assert!(e.contains("must specify one of"), "{e}");
    }

    #[test]
    fn invalid_name_rejected() {
        let e = Manifest::parse("ascript.toml", "[package]\nname = \"Bad_Name\"\nversion = \"1.0.0\"\n")
            .unwrap_err();
        assert!(e.contains("invalid package name"), "{e}");
    }

    #[test]
    fn scoped_name_accepted() {
        let m = Manifest::parse(
            "ascript.toml",
            "[package]\nname = \"@acme/schema\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        assert_eq!(m.package.unwrap().name, "@acme/schema");
    }

    #[test]
    fn version_must_be_triple() {
        let e = Manifest::parse(
            "ascript.toml",
            "[package]\nname = \"x\"\nversion = \"1.2\"\n",
        )
        .unwrap_err();
        assert!(e.contains("version") && e.contains("MAJOR.MINOR.PATCH"), "{e}");
    }

    #[test]
    fn version_rejects_range() {
        assert!(Version::parse("^1.2.0").is_err());
        assert!(Version::parse("1.2.0-alpha").is_err());
        assert!(Version::parse("1.2.0+build").is_err());
        assert!(Version::parse("1.2").is_err());
    }

    #[test]
    fn version_accepts_v_prefix() {
        assert_eq!(
            Version::parse("v2.0.1").unwrap(),
            Version {
                major: 2,
                minor: 0,
                patch: 1
            }
        );
    }

    #[test]
    fn version_ordering() {
        assert!(Version::parse("1.2.0").unwrap() < Version::parse("1.10.0").unwrap());
        assert!(Version::parse("2.0.0").unwrap() > Version::parse("1.9.9").unwrap());
    }

    #[test]
    fn malformed_toml_is_error_naming_file() {
        let e = Manifest::parse("ascript.toml", "[package\n").unwrap_err();
        assert!(e.contains("ascript.toml") && e.contains("malformed"), "{e}");
    }

    #[test]
    fn name_validation_table() {
        assert!(is_valid_name("http"));
        assert!(is_valid_name("as-http"));
        assert!(is_valid_name("a1"));
        assert!(is_valid_name("@acme/schema"));
        assert!(is_valid_name("@a-b/c-d"));
        assert!(!is_valid_name("Http"));
        assert!(!is_valid_name("-http"));
        assert!(!is_valid_name("ht_tp"));
        assert!(!is_valid_name("@/x"));
        assert!(!is_valid_name("@scope/"));
        assert!(!is_valid_name("@sc ope/x"));
        assert!(!is_valid_name(""));
    }
}
