//! Go-style Minimal Version Selection over the dependency graph (SP6 §3).
//!
//! MVS picks, per package name, the MAXIMUM of the MINIMUMS — the highest version
//! that appears as a REQUIREMENT across the whole graph (NOT the highest available
//! upstream tag). That is the reproducibility property: adding dep X never floats
//! an unrelated dep Y forward; only an explicit `ascript update` raises a
//! requirement.
//!
//! - **git-tag** deps are versioned: the tag is the declared version; multiple
//!   requirements on the same name resolve to the highest.
//! - **git-rev / url / path** deps are NON-versioned LEAVES — each is taken as-is;
//!   two requirements pinning the same name to DIFFERENT rev/url/path is a
//!   CONFLICT (single version per name, MVS-style), reported naming both requirers.
//! - A **bare-version** (registry) requirement → the clean "needs a registry" error.
//! - **Cycles** in the dependency graph are detected and reported with the path.
//!
//! The graph walk is decoupled from IO via the [`DepFetcher`] trait, so the core
//! algorithm is unit-tested PURELY (no network/git), and the real driver wires in
//! `fetch.rs`.

use super::manifest::{DepSource, GitPin, Version};
use std::collections::BTreeMap;

/// The metadata a fetch yields for one package, fed back into the resolver so it
/// can record lock fields and read transitive requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDep {
    /// The version recorded as `resolved` (tag/rev for git, fetched version for
    /// url, path string for path).
    pub resolved: String,
    /// The exact git commit (git deps only).
    pub rev: Option<String>,
    /// `asum1-…` integrity (git/url; `None` for path).
    pub integrity: Option<String>,
    /// The fetched package's OWN `[dependencies]` (transitive requirements).
    pub deps: Vec<(String, DepSource)>,
}

/// Abstracts "acquire a dependency and read its transitive deps", so the MVS walk
/// is testable without IO. The real impl fetches via `fetch.rs`.
pub trait DepFetcher {
    /// Fetch `src` (declared as `name`) and return its metadata + transitive deps.
    fn fetch(&mut self, name: &str, src: &DepSource) -> Result<FetchedDep, String>;
}

/// One fully-resolved package in the flat output set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub name: String,
    /// The winning source (after MVS selection for a versioned name).
    pub source: DepSource,
    /// The git tag requirement that won (for the lock `requirement` field), if the
    /// winning source is a git-tag dep.
    pub requirement: Option<String>,
    pub resolved: String,
    pub rev: Option<String>,
    pub integrity: Option<String>,
}

/// Internal per-name selection state during the walk.
struct Selection {
    /// The currently-selected source for this name.
    source: DepSource,
    /// For a versioned (git-tag) selection: the highest required version so far.
    version: Option<Version>,
    /// Who first required this name (for conflict messages).
    requirer: String,
    /// Cached fetch metadata for the selected source.
    fetched: FetchedDep,
}

/// Run MVS over `root_deps` (the root manifest's `[dependencies]`), using
/// `fetcher` to acquire each dep + read its transitive requirements. Returns the
/// flat resolved set sorted by name, or a clear conflict / cycle / registry error.
pub fn resolve(
    root_deps: &[(String, DepSource)],
    fetcher: &mut dyn DepFetcher,
) -> Result<Vec<Resolved>, String> {
    let mut selected: BTreeMap<String, Selection> = BTreeMap::new();
    // `stack` carries the active requirer chain for cycle detection + messages.
    let mut stack: Vec<String> = Vec::new();
    walk("(root)", root_deps, fetcher, &mut selected, &mut stack)?;

    let mut out: Vec<Resolved> = selected
        .into_iter()
        .map(|(name, sel)| {
            let requirement = match &sel.source {
                DepSource::Git {
                    pin: GitPin::Tag(t),
                    ..
                } => Some(t.clone()),
                _ => None,
            };
            Resolved {
                name,
                source: sel.source,
                requirement,
                resolved: sel.fetched.resolved,
                rev: sel.fetched.rev,
                integrity: sel.fetched.integrity,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Process `deps` required by `requirer`, recursing into each dep's transitive
/// requirements (depth-first). `stack` holds the active requirer-name chain.
fn walk(
    requirer: &str,
    deps: &[(String, DepSource)],
    fetcher: &mut dyn DepFetcher,
    selected: &mut BTreeMap<String, Selection>,
    stack: &mut Vec<String>,
) -> Result<(), String> {
    for (name, src) in deps {
        // A bare-version (registry) requirement is unsupported in SP6.
        if let DepSource::Registry { req } = src {
            return Err(format!(
                "bare-version dependency '{name}' (\"{req}\") requires a registry, \
                 which is not available yet"
            ));
        }

        // Cycle: this name is already on the active requirer chain.
        if stack.iter().any(|n| n == name) {
            let mut cycle = stack.clone();
            cycle.push(name.clone());
            return Err(format!(
                "dependency cycle detected: {}",
                cycle.join(" -> ")
            ));
        }

        match selected.get(name) {
            None => {
                // First time we see this name: fetch + select + recurse.
                let fetched = fetcher.fetch(name, src)?;
                let version = tag_version(name, src)?;
                let transitive = fetched.deps.clone();
                selected.insert(
                    name.clone(),
                    Selection {
                        source: src.clone(),
                        version,
                        requirer: requirer.to_string(),
                        fetched,
                    },
                );
                stack.push(name.clone());
                walk(name, &transitive, fetcher, selected, stack)?;
                stack.pop();
            }
            Some(existing) => {
                // Already selected: MVS-merge. For two git-tag requirements, keep
                // the higher version (re-fetch only if the new one wins). For
                // non-versioned leaves, the sources must be IDENTICAL or it's a
                // conflict.
                let new_version = tag_version(name, src)?;
                match (existing.version, new_version) {
                    (Some(cur), Some(new)) if matches!(src, DepSource::Git { .. }) => {
                        // Both are git-tag on the same git url? Require the same url.
                        let same_url = same_git_url(&existing.source, src);
                        if !same_url {
                            return Err(conflict_msg(
                                name,
                                &existing.requirer,
                                &existing.source,
                                requirer,
                                src,
                            ));
                        }
                        if new > cur {
                            // The new requirement wins: re-fetch at the higher tag
                            // and recurse into ITS transitive deps.
                            let fetched = fetcher.fetch(name, src)?;
                            let transitive = fetched.deps.clone();
                            selected.insert(
                                name.clone(),
                                Selection {
                                    source: src.clone(),
                                    version: Some(new),
                                    requirer: requirer.to_string(),
                                    fetched,
                                },
                            );
                            stack.push(name.clone());
                            walk(name, &transitive, fetcher, selected, stack)?;
                            stack.pop();
                        }
                        // else: keep the existing (higher-or-equal) selection.
                    }
                    _ => {
                        // At least one side is a non-versioned leaf (rev/url/path):
                        // the sources must be identical, else conflict.
                        if !sources_equal(&existing.source, src) {
                            return Err(conflict_msg(
                                name,
                                &existing.requirer,
                                &existing.source,
                                requirer,
                                src,
                            ));
                        }
                        // Identical leaf re-requirement: nothing to do (already
                        // selected + its transitive deps already walked).
                    }
                }
            }
        }
    }
    Ok(())
}

/// The declared version of a git-TAG dep (the MVS comparison unit); `None` for
/// non-versioned leaves (rev/url/path). A non-conforming tag is a clear error.
fn tag_version(name: &str, src: &DepSource) -> Result<Option<Version>, String> {
    match src {
        DepSource::Git {
            pin: GitPin::Tag(t),
            ..
        } => Version::parse(t)
            .map(Some)
            .map_err(|e| format!("git dependency '{name}': tag '{t}' is not a version: {e}")),
        _ => Ok(None),
    }
}

/// Whether two git sources point at the same url (tag-vs-tag merge precondition).
fn same_git_url(a: &DepSource, b: &DepSource) -> bool {
    match (a, b) {
        (DepSource::Git { url: ua, .. }, DepSource::Git { url: ub, .. }) => ua == ub,
        _ => false,
    }
}

/// Structural source equality (for non-versioned leaf conflict detection).
fn sources_equal(a: &DepSource, b: &DepSource) -> bool {
    a == b
}

/// A conflict error naming BOTH requirers and the two incompatible sources.
fn conflict_msg(
    name: &str,
    req_a: &str,
    src_a: &DepSource,
    req_b: &str,
    src_b: &DepSource,
) -> String {
    format!(
        "dependency conflict on '{name}': {req_a} requires {} but {req_b} requires {} \
         (a single version per package is selected; reconcile the two)",
        describe(src_a),
        describe(src_b)
    )
}

/// Human description of a source for error messages.
fn describe(src: &DepSource) -> String {
    match src {
        DepSource::Git {
            url,
            pin: GitPin::Tag(t),
        } => format!("git {url}@{t}"),
        DepSource::Git {
            url,
            pin: GitPin::Rev(r),
        } => format!("git {url}@{r}"),
        DepSource::Url { url } => format!("url {url}"),
        DepSource::Path { path } => format!("path {path}"),
        DepSource::Registry { req } => format!("registry {req}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A pure in-memory fetcher: maps a source to its transitive deps, with the
    /// `resolved`/integrity derived deterministically. No IO.
    struct MockFetcher {
        /// keyed by a stable string id of the source.
        graph: HashMap<String, Vec<(String, DepSource)>>,
    }

    impl MockFetcher {
        fn new() -> Self {
            MockFetcher {
                graph: HashMap::new(),
            }
        }
        fn add(&mut self, src: &DepSource, deps: Vec<(String, DepSource)>) {
            self.graph.insert(key(src), deps);
        }
    }

    fn key(src: &DepSource) -> String {
        super::describe(src)
    }

    impl DepFetcher for MockFetcher {
        fn fetch(&mut self, _name: &str, src: &DepSource) -> Result<FetchedDep, String> {
            let deps = self.graph.get(&key(src)).cloned().unwrap_or_default();
            let resolved = match src {
                DepSource::Git { pin: GitPin::Tag(t), .. } => t.clone(),
                DepSource::Git { pin: GitPin::Rev(r), .. } => r.clone(),
                DepSource::Url { url } => url.clone(),
                DepSource::Path { path } => path.clone(),
                DepSource::Registry { req } => req.clone(),
            };
            let integrity = if matches!(src, DepSource::Path { .. }) {
                None
            } else {
                Some("asum1-x".to_string())
            };
            Ok(FetchedDep {
                resolved,
                rev: matches!(src, DepSource::Git { .. }).then(|| "deadbeef".to_string()),
                integrity,
                deps,
            })
        }
    }

    fn git_tag(url: &str, tag: &str) -> DepSource {
        DepSource::Git {
            url: url.into(),
            pin: GitPin::Tag(tag.into()),
        }
    }

    #[test]
    fn max_of_mins_across_direct_and_transitive() {
        // root -> a@1.0.0, root -> b@1.0.0; b -> a@1.2.0. MVS selects a@1.2.0.
        let url_a = "https://x/a";
        let mut m = MockFetcher::new();
        m.add(&git_tag(url_a, "1.0.0"), vec![]);
        m.add(&git_tag(url_a, "1.2.0"), vec![]);
        m.add(
            &git_tag("https://x/b", "1.0.0"),
            vec![("a".into(), git_tag(url_a, "1.2.0"))],
        );
        let roots = vec![
            ("a".into(), git_tag(url_a, "1.0.0")),
            ("b".into(), git_tag("https://x/b", "1.0.0")),
        ];
        let out = resolve(&roots, &mut m).unwrap();
        let a = out.iter().find(|r| r.name == "a").unwrap();
        assert_eq!(a.resolved, "1.2.0", "MVS picks the highest required");
    }

    #[test]
    fn lower_transitive_does_not_downgrade() {
        // root -> a@1.5.0; root -> b; b -> a@1.0.0. Selected stays a@1.5.0.
        let url_a = "https://x/a";
        let mut m = MockFetcher::new();
        m.add(&git_tag(url_a, "1.5.0"), vec![]);
        m.add(&git_tag(url_a, "1.0.0"), vec![]);
        m.add(
            &git_tag("https://x/b", "1.0.0"),
            vec![("a".into(), git_tag(url_a, "1.0.0"))],
        );
        let roots = vec![
            ("a".into(), git_tag(url_a, "1.5.0")),
            ("b".into(), git_tag("https://x/b", "1.0.0")),
        ];
        let out = resolve(&roots, &mut m).unwrap();
        let a = out.iter().find(|r| r.name == "a").unwrap();
        assert_eq!(a.resolved, "1.5.0");
    }

    #[test]
    fn non_versioned_leaf_taken_as_is() {
        let mut m = MockFetcher::new();
        let src = DepSource::Path { path: "../u".into() };
        m.add(&src, vec![]);
        let out = resolve(&[("u".into(), src)], &mut m).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].resolved, "../u");
        assert!(out[0].integrity.is_none(), "path dep has no integrity");
    }

    #[test]
    fn conflicting_path_same_name_errors_naming_both() {
        // root -> u (path ../u1); root -> b; b -> u (path ../u2). Conflict.
        let mut m = MockFetcher::new();
        let u1 = DepSource::Path { path: "../u1".into() };
        let u2 = DepSource::Path { path: "../u2".into() };
        m.add(&u1, vec![]);
        m.add(&u2, vec![]);
        m.add(
            &git_tag("https://x/b", "1.0.0"),
            vec![("u".into(), u2.clone())],
        );
        let roots = vec![
            ("u".into(), u1),
            ("b".into(), git_tag("https://x/b", "1.0.0")),
        ];
        let e = resolve(&roots, &mut m).unwrap_err();
        assert!(e.contains("conflict on 'u'"), "{e}");
        assert!(e.contains("(root)") && e.contains("b"), "names both requirers: {e}");
        assert!(e.contains("../u1") && e.contains("../u2"), "names both sources: {e}");
    }

    #[test]
    fn conflicting_rev_same_name_errors() {
        let mut m = MockFetcher::new();
        let r1 = DepSource::Git { url: "https://x/c".into(), pin: GitPin::Rev("aaa".into()) };
        let r2 = DepSource::Git { url: "https://x/c".into(), pin: GitPin::Rev("bbb".into()) };
        m.add(&r1, vec![]);
        m.add(&r2, vec![]);
        m.add(&git_tag("https://x/b", "1.0.0"), vec![("c".into(), r2)]);
        let roots = vec![
            ("c".into(), r1),
            ("b".into(), git_tag("https://x/b", "1.0.0")),
        ];
        let e = resolve(&roots, &mut m).unwrap_err();
        assert!(e.contains("conflict on 'c'"), "{e}");
    }

    #[test]
    fn cycle_detected_with_path() {
        // a -> b -> a.
        let mut m = MockFetcher::new();
        m.add(
            &git_tag("https://x/a", "1.0.0"),
            vec![("b".into(), git_tag("https://x/b", "1.0.0"))],
        );
        m.add(
            &git_tag("https://x/b", "1.0.0"),
            vec![("a".into(), git_tag("https://x/a", "1.0.0"))],
        );
        let roots = vec![("a".into(), git_tag("https://x/a", "1.0.0"))];
        let e = resolve(&roots, &mut m).unwrap_err();
        assert!(e.contains("cycle"), "{e}");
        assert!(e.contains("a -> b -> a"), "cycle path: {e}");
    }

    #[test]
    fn registry_requirement_needs_a_registry() {
        let mut m = MockFetcher::new();
        let roots = vec![(
            "color".into(),
            DepSource::Registry { req: "^1.2.0".into() },
        )];
        let e = resolve(&roots, &mut m).unwrap_err();
        assert!(e.contains("requires a registry"), "{e}");
    }

    #[test]
    fn identical_leaf_required_twice_is_ok() {
        // root -> u (../u); root -> b; b -> u (../u). Same path → no conflict.
        let mut m = MockFetcher::new();
        let u = DepSource::Path { path: "../u".into() };
        m.add(&u, vec![]);
        m.add(&git_tag("https://x/b", "1.0.0"), vec![("u".into(), u.clone())]);
        let roots = vec![
            ("u".into(), u),
            ("b".into(), git_tag("https://x/b", "1.0.0")),
        ];
        let out = resolve(&roots, &mut m).unwrap();
        assert_eq!(out.iter().filter(|r| r.name == "u").count(), 1);
    }

    #[test]
    fn output_sorted_by_name() {
        let mut m = MockFetcher::new();
        let z = DepSource::Path { path: "../z".into() };
        let a = DepSource::Path { path: "../a".into() };
        m.add(&z, vec![]);
        m.add(&a, vec![]);
        let out = resolve(&[("zeta".into(), z), ("alpha".into(), a)], &mut m).unwrap();
        assert_eq!(out[0].name, "alpha");
        assert_eq!(out[1].name, "zeta");
    }
}
