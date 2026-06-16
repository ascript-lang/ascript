//! DX D2 Task 10 — `ascript test --watch` (spec §6.4): re-run the affected tests when a
//! source file changes, SCOPING by the workspace import graph so only test files whose
//! import closure touched the changed file re-run.
//!
//! ## What is pure vs. what is the loop
//!
//! The interesting (and risky) part — *given a changed file + the import graph, which test
//! files must re-run?* — is factored into the PURE [`affected_test_files`] function and
//! unit-tested below. The actual watch LOOP (poll mtimes → on a change, compute the affected
//! set → re-run via the existing parallel runner) is a thin `sys`-gated wrapper in
//! [`run_watch`]; it never terminates (runs until Ctrl-C), so it is deliberately NOT
//! wrapped in a test that would hang. We use mtime POLLING (no new always-on crate
//! dependency).
//!
//! ## Import graph model
//!
//! [`ImportGraph`] is a minimal, interpreter-free, `lsp`-independent reverse-edge map built
//! by parsing each file with the (core) CST front-end and resolving its relative imports.
//! It is the same dependency information the LSP `WorkspaceIndex` maintains
//! (`ImportEdge`/`importers`), reduced to exactly what scoping needs. If the graph can't be
//! built (a parse error, an unreadable file), the loop FALLS BACK to "run all given files".
//!
//! ## Explicit deferral
//!
//! In-source `test.only(name, fn)` / `test.skip(name, fn)` markers are an EXPLICIT DEFERRAL
//! (spec §6.6 / §10) — NOT implemented here. `--filter` (a flag on the registration set) is
//! the first-cut focus mechanism; per-test source markers are an additive follow-up.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A reverse-import dependency graph over a set of `.as` files: for each file, the set of
/// files that DIRECTLY import it. The transitive closure over these reverse edges answers
/// "which files are affected when X changes?".
#[derive(Debug, Default, Clone)]
pub struct ImportGraph {
    /// `module path -> the files that directly import it` (reverse edges, like the LSP
    /// `WorkspaceIndex.importers`). All paths are lexically-canonicalized absolute paths.
    importers: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl ImportGraph {
    /// An empty graph (no edges) — every changed file affects only itself.
    pub fn new() -> ImportGraph {
        ImportGraph::default()
    }

    /// Record that `importer` imports `target` (a reverse edge `target -> importer`).
    pub fn add_edge(&mut self, importer: PathBuf, target: PathBuf) {
        self.importers.entry(target).or_default().insert(importer);
    }

    /// Build a graph by parsing each file in `files` with the core CST front-end and
    /// resolving its RELATIVE imports (`std/*` and bare-package imports have no file edge).
    /// Best-effort: an unreadable / unparsable file contributes no edges (the loop's
    /// fallback covers a degraded graph). Paths are lexically canonicalized so edges line
    /// up with the changed-file lookup. Never panics.
    pub fn build(files: &[PathBuf]) -> ImportGraph {
        let mut g = ImportGraph::new();
        for file in files {
            let canon = lexical_canonicalize(file);
            let Ok(text) = std::fs::read_to_string(&canon) else {
                continue;
            };
            let dir = canon.parent().map(Path::to_path_buf).unwrap_or_default();
            for spec in parse_relative_imports(&text) {
                if let Some(target) = resolve_relative(&spec, &dir) {
                    g.add_edge(canon.clone(), target);
                }
            }
        }
        g
    }
}

/// Pure scoping: given the file that CHANGED and the import graph, return the set of
/// `candidate` test files that must re-run — the changed file itself (if it is a candidate)
/// plus every candidate whose import closure (transitively) reaches the changed file.
///
/// The result is the candidates ∩ (changed-file ∪ transitive importers-of changed-file),
/// returned SORTED for deterministic re-run order. If the changed file affects no candidate
/// (e.g. it is outside the corpus and nothing imports it), the result is empty — the caller
/// decides whether an empty set means "skip" or "fall back to all".
pub fn affected_test_files(
    changed: &Path,
    graph: &ImportGraph,
    candidates: &[PathBuf],
) -> Vec<PathBuf> {
    let changed = lexical_canonicalize(changed);
    let candidate_set: HashSet<PathBuf> =
        candidates.iter().map(|p| lexical_canonicalize(p)).collect();

    // BFS over reverse edges from `changed`: every file transitively importing `changed`.
    let mut affected: HashSet<PathBuf> = HashSet::new();
    let mut stack = vec![changed.clone()];
    affected.insert(changed);
    while let Some(node) = stack.pop() {
        if let Some(importers) = graph.importers.get(&node) {
            for imp in importers {
                if affected.insert(imp.clone()) {
                    stack.push(imp.clone());
                }
            }
        }
    }

    let mut out: Vec<PathBuf> = affected.intersection(&candidate_set).cloned().collect();
    out.sort();
    out
}

/// Lexically canonicalize a path WITHOUT touching the filesystem (so it works on a path
/// that may not exist yet and never blocks): make absolute against the cwd, then fold away
/// `.` / `..` components. Matches the LSP workspace's lexical-canonicalization posture.
fn lexical_canonicalize(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Extract the RELATIVE import specifiers (`./m`, `../m`, `/abs/m`) of a source file via
/// the core CST parser. `std/*` and bare-package specifiers are dropped (no file edge).
/// Best-effort on a parse error (returns whatever import statements parsed). Never panics.
fn parse_relative_imports(text: &str) -> Vec<String> {
    use crate::syntax::kind::SyntaxKind;
    let tree = crate::syntax::parse_to_tree(text);
    let mut specs = Vec::new();
    for child in tree.children() {
        // Unwrap a leading `export`.
        let decl = if child.kind() == SyntaxKind::ExportStmt {
            match child.children().next() {
                Some(d) => d.clone(),
                None => continue,
            }
        } else {
            child.clone()
        };
        if decl.kind() != SyntaxKind::ImportStmt {
            continue;
        }
        if let Some(spec) = import_specifier(&decl) {
            if is_relative_specifier(&spec) {
                specs.push(spec);
            }
        }
    }
    specs
}

/// The quote-stripped `from "<spec>"` string of an `ImportStmt`, or `None`.
fn import_specifier(import: &crate::syntax::cst::ResolvedNode) -> Option<String> {
    use crate::syntax::kind::SyntaxKind;
    let tok = import
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Str)?;
    let raw = tok.text();
    Some(
        raw.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw)
            .to_string(),
    )
}

/// A relative/absolute file specifier (`./`, `../`, `/`) — NOT a `std/*` or bare package.
fn is_relative_specifier(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/')
}

/// Resolve a relative specifier against the importer's `dir` to a canonical file path,
/// adding a default `.as` extension when the specifier has none.
fn resolve_relative(spec: &str, dir: &Path) -> Option<PathBuf> {
    let mut joined = dir.join(spec);
    if joined.extension().is_none() {
        joined.set_extension("as");
    }
    Some(lexical_canonicalize(&joined))
}

// ---------------------------------------------------------------------------------------
// The watch LOOP (sys-gated): poll mtimes, re-run the affected set on change. Never
// terminates (runs until Ctrl-C), so it is intentionally not unit-tested — the risky
// scoping logic above is. This is a thin wrapper over the existing parallel runner.
// ---------------------------------------------------------------------------------------

/// Run the test corpus once, then WATCH for changes and re-run the affected subset until
/// interrupted. `sys`-gated (mtime polling needs filesystem metadata + a steady-state
/// loop). The graph is built once up front from `files`; if it is empty / a change touches
/// nothing in it, we fall back to re-running ALL `files`.
#[cfg(feature = "sys")]
pub async fn run_watch(
    files: &[String],
    packages: Option<crate::interp::PackageMap>,
    caps: Option<crate::stdlib::caps::CapSet>,
    parallel: Option<usize>,
    filter: Option<&str>,
) -> Result<(), crate::error::AsError> {
    use std::time::Duration;

    let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    let graph = ImportGraph::build(&paths);

    // Initial full run. ELIDE §5: honor `ASCRIPT_ELIDE`/`ASCRIPT_NO_ELIDE` over the
    // measured default (the `--watch` surface has no elide flags of its own).
    let elide = crate::elide_enabled(false, false);
    print_run_summary(
        crate::run_tests_with_options(
            files,
            packages.clone(),
            caps.clone(),
            parallel,
            false,
            filter,
            elide,
        )
        .await,
    );
    eprintln!("watching {} file(s) for changes (Ctrl-C to stop)…", files.len());

    // Seed the mtime table for every corpus file + every file they import (transitively
    // captured via the graph's known keys). We watch the union of corpus files and any
    // file that appears as an import target in the graph.
    let mut watched: Vec<PathBuf> = paths.iter().map(|p| lexical_canonicalize(p)).collect();
    for target in graph.importers.keys() {
        if !watched.contains(target) {
            watched.push(target.clone());
        }
    }
    let mut mtimes: HashMap<PathBuf, Option<std::time::SystemTime>> =
        watched.iter().map(|p| (p.clone(), mtime_of(p))).collect();

    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Collect every watched file whose mtime advanced since last poll.
        let mut changed: Vec<PathBuf> = Vec::new();
        for path in &watched {
            let now = mtime_of(path);
            let prev = mtimes.get(path).cloned().flatten();
            if now != prev {
                mtimes.insert(path.clone(), now);
                // A deleted-then-recreated or modified file both register as a change.
                if now.is_some() {
                    changed.push(path.clone());
                }
            }
        }
        if changed.is_empty() {
            continue;
        }

        // Compute the affected candidate (corpus) files across ALL changes; fall back to
        // the full corpus when the graph yields nothing (an untracked or graph-less change).
        let mut affected: HashSet<PathBuf> = HashSet::new();
        for c in &changed {
            for a in affected_test_files(c, &graph, &paths) {
                affected.insert(a);
            }
        }
        let rerun: Vec<String> = if affected.is_empty() {
            files.to_vec()
        } else {
            // Preserve the corpus input order for deterministic output.
            paths
                .iter()
                .filter(|p| affected.contains(&lexical_canonicalize(p)))
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        };

        eprintln!("change detected — re-running {} file(s)…", rerun.len());
        print_run_summary(
            crate::run_tests_with_options(
                &rerun,
                packages.clone(),
                caps.clone(),
                parallel,
                false,
                filter,
                elide,
            )
            .await,
        );
    }
}

/// Print a test-run summary the same way the one-shot `test` dispatch does (so a watch
/// re-run looks identical to a plain run). `sys`-gated alongside the loop.
#[cfg(feature = "sys")]
fn print_run_summary(result: Result<crate::TestSummary, crate::error::AsError>) {
    match result {
        Ok(summary) => {
            for (name, message) in &summary.failures {
                println!("FAIL {name}: {message}");
            }
            summary.print_tally();
        }
        Err(e) => crate::diagnostics::report(&e),
    }
}

/// The last-modified time of a path, or `None` if it can't be stat'd (deleted / unreadable).
#[cfg(feature = "sys")]
fn mtime_of(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        // Use the lexical-canonical form so the test asserts against the same key space the
        // scoping function uses (absolute, cwd-rooted).
        lexical_canonicalize(Path::new(s))
    }

    /// A changed leaf module re-runs every test file whose import closure reaches it
    /// (transitively), and ONLY those — an unrelated test file is NOT re-run.
    #[test]
    fn affected_set_is_the_transitive_importers_intersect_candidates() {
        // Graph: test_a imports util; test_b imports helper imports util; test_c imports nothing.
        let mut g = ImportGraph::new();
        g.add_edge(p("/w/test_a.as"), p("/w/util.as"));
        g.add_edge(p("/w/helper.as"), p("/w/util.as"));
        g.add_edge(p("/w/test_b.as"), p("/w/helper.as"));

        let candidates = vec![p("/w/test_a.as"), p("/w/test_b.as"), p("/w/test_c.as")];

        // Changing util.as affects test_a (direct) and test_b (via helper) — NOT test_c.
        let got = affected_test_files(Path::new("/w/util.as"), &g, &candidates);
        assert_eq!(got, vec![p("/w/test_a.as"), p("/w/test_b.as")]);

        // Changing helper.as affects only test_b (test_a doesn't import helper).
        let got = affected_test_files(Path::new("/w/helper.as"), &g, &candidates);
        assert_eq!(got, vec![p("/w/test_b.as")]);
    }

    /// A changed candidate file with no importers re-runs only ITSELF.
    #[test]
    fn changed_candidate_with_no_importers_runs_itself() {
        let g = ImportGraph::new();
        let candidates = vec![p("/w/test_a.as"), p("/w/test_b.as")];
        let got = affected_test_files(Path::new("/w/test_a.as"), &g, &candidates);
        assert_eq!(got, vec![p("/w/test_a.as")]);
    }

    /// A changed file outside the corpus that nothing imports affects NO candidate (empty —
    /// the caller decides fallback). Distinct from "itself" because it is not a candidate.
    #[test]
    fn unrelated_change_affects_nothing() {
        let mut g = ImportGraph::new();
        g.add_edge(p("/w/test_a.as"), p("/w/util.as"));
        let candidates = vec![p("/w/test_a.as")];
        let got = affected_test_files(Path::new("/w/stranger.as"), &g, &candidates);
        assert!(got.is_empty(), "got: {got:?}");
    }

    /// A cycle in the import graph terminates (no infinite traversal) and yields the cycle
    /// members that are candidates.
    #[test]
    fn import_cycle_terminates() {
        let mut g = ImportGraph::new();
        // a imports b, b imports a (a degenerate cycle).
        g.add_edge(p("/w/a.as"), p("/w/b.as"));
        g.add_edge(p("/w/b.as"), p("/w/a.as"));
        let candidates = vec![p("/w/a.as"), p("/w/b.as")];
        let mut got = affected_test_files(Path::new("/w/a.as"), &g, &candidates);
        got.sort();
        assert_eq!(got, vec![p("/w/a.as"), p("/w/b.as")]);
    }

    /// `ImportGraph::build` resolves relative imports from real source (the parser path),
    /// dropping `std/*`. Exercises the build + scoping end to end against temp files.
    #[test]
    fn build_resolves_relative_imports_and_drops_std() {
        let dir = std::env::temp_dir().join(format!("ascript_watch_build_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let util = dir.join("util.as");
        let test_a = dir.join("test_a.as");
        std::fs::write(&util, "export fn helper() { return 1 }\n").unwrap();
        std::fs::write(
            &test_a,
            "import { helper } from \"./util\"\n\
             import { abs } from \"std/math\"\n\
             test(\"t\", () => { assert(helper() == 1) })\n",
        )
        .unwrap();

        let files = vec![util.clone(), test_a.clone()];
        let g = ImportGraph::build(&files);
        // Changing util.as must affect test_a.as via the resolved `./util` edge; the
        // `std/math` import contributes no file edge.
        let got = affected_test_files(&util, &g, &files);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            got.contains(&lexical_canonicalize(&test_a)),
            "test_a must be affected by a util.as change: {got:?}"
        );
    }
}
