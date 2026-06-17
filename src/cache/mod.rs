//! Compile cache — module-graph walk and content-addressed cache for `ascript run`.
//!
//! This module is CLI-side only: it lives outside the core runtime and
//! introduces no new engine dependencies. It builds under `--no-default-features`.
//!
//! See WARM spec §2 for the design.

pub mod compile_cache;

use crate::error::AsError;
use std::path::{Path, PathBuf};

/// One module in the reachable import graph, as the BFS walk discovers it.
/// Produced by [`collect_module_graph`] and consumed by both the compile-cache
/// keyer (to hash sources) and `compile_archive`/`compile_archive_with_shake`
/// (to know the compiled set).
pub struct GraphModule {
    /// The archive-style logical key (`join_logical` convention from `archive.rs`).
    pub logical_key: String,
    /// The canonical on-disk absolute path (the dedup identity).
    pub path: PathBuf,
    /// The full UTF-8 source text as read from disk.
    pub source: String,
}

/// Walk the import graph from `entry` (the same BFS enumeration that
/// `compile_archive` uses), WITHOUT compiling to bytecode.
///
/// Returns the ordered list of reachable modules (entry first), with the
/// source text of each. Any IO / parse error is returned so the CACHE caller
/// can fail open (fall through to the uncached compile path); a compile error
/// in a transitive module is surfaced as a normal `AsError` so the caller's
/// uncached path will also error.
///
/// The walk uses `Interp::classify_specifier` (the same resolver the archive
/// builder uses) to resolve import specifiers so the keyed set and the compiled
/// set are identical by construction.
pub fn collect_module_graph(entry: &Path) -> Result<Vec<GraphModule>, AsError> {
    use crate::interp::{Interp, SpecifierKind};
    use crate::vm::archive::{join_logical, logical_parent};
    use std::collections::HashMap;

    /// BFS work item: on-disk path, archive logical key, and the logical dir
    /// the module's OWN imports resolve against.
    struct Pending {
        path: PathBuf,
        key: String,
        logical_dir: String,
    }

    // The `Interp` is used ONLY as the host for `classify_specifier` — no code runs.
    let interp = Interp::new();

    let entry_canon = entry
        .canonicalize()
        .map_err(|e| AsError::new(format!("cannot read {}: {}", entry.display(), e)))?;
    let entry_dir = entry_canon
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let entry_key = entry_canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry.as".to_string());

    let mut result: Vec<GraphModule> = Vec::new();
    // Canonical path → index in `result` (dedup identity, cycle terminator).
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    let mut queue: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();

    queue.push_back(Pending {
        path: entry_canon.clone(),
        key: entry_key,
        logical_dir: String::new(),
    });
    // Reserve slot 0 for the entry (BFS processes it first).
    seen.insert(entry_canon.clone(), 0);

    while let Some(item) = queue.pop_front() {
        // Read source from disk.
        let source = std::fs::read_to_string(&item.path).map_err(|e| {
            AsError::new(format!("cannot read {}: {}", item.path.display(), e))
        })?;

        // Compile to bytecode only to read the import table — we need the resolved
        // specifiers in the same form `compile_archive_with_shake` uses. We cannot
        // use the legacy parser's import AST because WARM must be byte-identical to
        // what compile_archive does (same specifier normalization, same error path).
        let aso_bytes = crate::compile_verified_aso_bytes_from_source_for_cache(
            &item.path,
            &source,
        )?;
        let chunk = crate::vm::chunk::Chunk::from_bytes_verified(&aso_bytes).map_err(|e| {
            AsError::new(format!(
                "internal: re-decoding compiled module {} failed: {e:?}",
                item.path.display()
            ))
        })?;

        let this_disk_dir = item
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| entry_dir.clone());

        // Follow imports to discover new modules (same logic as compile_archive_with_shake).
        for imp in &chunk.imports {
            let source_spec = imp.source();
            interp.set_module_dir(this_disk_dir.clone());
            match interp.classify_specifier(source_spec) {
                SpecifierKind::Std => {
                    // Native stdlib — linked in, never a file on disk.
                }
                kind @ (SpecifierKind::Relative(_) | SpecifierKind::Package { .. }) => {
                    let target = match &kind {
                        SpecifierKind::Relative(t) => t.clone(),
                        SpecifierKind::Package { target, .. } => target.clone(),
                        _ => unreachable!(),
                    };
                    let dep_path =
                        crate::resolve_module_file_pub(&target).map_err(|msg| {
                            AsError::new(format!(
                                "cannot resolve import '{source_spec}' from {}: {msg}",
                                item.path.display()
                            ))
                        })?;
                    if !seen.contains_key(&dep_path) {
                        let dep_key = match &kind {
                            SpecifierKind::Package { .. } => {
                                join_logical("pkg", source_spec)
                            }
                            _ => join_logical(&item.logical_dir, source_spec),
                        };
                        let dep_logical_dir = logical_parent(&dep_key);
                        let reserved = seen.len();
                        seen.insert(dep_path.clone(), reserved);
                        queue.push_back(Pending {
                            path: dep_path,
                            key: dep_key,
                            logical_dir: dep_logical_dir,
                        });
                    }
                }
                SpecifierKind::UnknownPackage(key) => {
                    return Err(AsError::new(format!(
                        "unknown package '{key}' — add it with 'ascript add' \
                         (imported from {})",
                        item.path.display()
                    )));
                }
            }
        }

        let idx = *seen
            .get(&item.path)
            .expect("every queued module is pre-registered in `seen`");
        // The BFS processes modules in the same order they were enqueued, so
        // `result.len()` always equals `idx` here.
        debug_assert_eq!(idx, result.len());
        result.push(GraphModule {
            logical_key: item.key,
            path: item.path,
            source,
        });
    }

    Ok(result)
}
