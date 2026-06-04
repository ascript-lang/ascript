//! Cross-file workspace index for the LSP (SP4 §4).
//!
//! Static-analysis-only: this reuses the SAME CST front-end the checker uses
//! (`syntax::lex`/`parse`/`tree_builder::build_tree`/`resolve::resolve`) and
//! projects the result into a cross-file symbol index. It holds ONLY
//! `String`/`PathBuf`/byte-range data — never an interpreter `Rc`/`RefCell`/
//! `Value` — so the whole layer stays `Send + Sync` and never instantiates the
//! interpreter.
//!
//! The index powers cross-file go-to-definition, workspace/document symbols,
//! find-references, and rename. It is warm + incremental: a file is re-indexed on
//! `didOpen`/`didChange`; a parse error retains the file's last-good index so
//! navigation degrades gracefully.

use crate::check::diagnostic::ByteSpan;
use crate::syntax::kind::SyntaxKind;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The kind of a defined symbol (a small, LSP-facing subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Fn,
    Class,
    Enum,
    Const,
    Let,
    Import,
}

/// A symbol definition: its name, kind, defining file, and the byte range of its
/// NAME token (for a precise go-to / rename target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub name: String,
    pub kind: DefKind,
    pub path: PathBuf,
    pub name_range: ByteSpan,
}

/// Where a name USE resolves (within the file's own resolution; the cross-file
/// link is computed via the import edge + the target file's `exports`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// Resolves to a definition in the SAME file (its name-range).
    LocalDef(ByteSpan),
    /// An imported name: `module` is the resolved file path (or `None` for std),
    /// `name` the imported symbol.
    Imported {
        module: Option<PathBuf>,
        name: String,
    },
    /// A `std/*` import or otherwise unresolved use.
    Other,
}

/// A name USE site in a file: the byte range of the use token + what it resolves
/// to. Drives cross-file go-to-def and find-references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseSite {
    pub range: ByteSpan,
    pub name: String,
    pub target: ResolvedTarget,
}

/// An import edge: `importer -> (specifier, resolved path, imported names)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEdge {
    pub specifier: String,
    /// The resolved target file path (`None` for `std/*` or an unresolved path).
    pub resolved: Option<PathBuf>,
    pub names: Vec<String>,
}

/// Per-file parsed + resolved facts, keyed by canonical path in the workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileIndex {
    pub text: String,
    /// Exported symbol name -> its def (from `export <decl>` statements).
    pub exports: HashMap<String, SymbolDef>,
    /// All top-level declarations in this file (the document-symbol set).
    pub defs: Vec<SymbolDef>,
    /// Resolved name-uses in this file (cross-file def + find-references source).
    pub uses: Vec<UseSite>,
    /// Import edges out of this file.
    pub imports: Vec<ImportEdge>,
    /// `true` if the file parsed without a syntax error. On a parse error the
    /// previous (last-good) `FileIndex` is retained, so this is only `false` for a
    /// freshly-added file that never parsed.
    pub parsed_ok: bool,
}

/// The cross-file symbol index over a set of `.as` files.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndex {
    /// Per-file facts, keyed by canonical path.
    pub files: HashMap<PathBuf, FileIndex>,
    /// Symbol name -> every defining `(path, range, kind)` across the workspace.
    pub defs_by_name: HashMap<String, Vec<SymbolDef>>,
    /// Import graph: importer path -> its edges.
    pub import_edges: HashMap<PathBuf, Vec<ImportEdge>>,
    /// Reverse edges: module path -> the set of files that import it.
    pub importers: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl WorkspaceIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an index over an in-memory set of `(path, text)` files (the unit-test
    /// and `did_open`-seeded entry point). Paths are canonicalized lexically (no fs
    /// access), so a relative import resolves deterministically against the
    /// importer's directory.
    pub fn build_from_files(files: &[(PathBuf, String)]) -> Self {
        let mut idx = WorkspaceIndex::new();
        for (path, text) in files {
            idx.reindex_file(path, text);
        }
        idx
    }

    /// Re-index a single file (incremental update on `didOpen`/`didChange`). On a
    /// parse error the file's LAST-GOOD index is retained (navigation degrades
    /// gracefully); a never-parsed file gets an empty `parsed_ok = false` entry.
    /// Updates the `defs_by_name` / `import_edges` / `importers` deltas for this
    /// file only — a cross-file use target is resolved lazily at query time via the
    /// import edge, so editing one file never invalidates another's own index.
    pub fn reindex_file(&mut self, path: &Path, text: &str) {
        let canon = canonicalize(path);
        let parsed = crate::syntax::parser::parse(text);
        let parsed_ok = parsed.errors.is_empty() && parsed.lex_errors.is_empty();

        if !parsed_ok {
            // Retain the last-good index if we have one; otherwise record an empty
            // placeholder so the file is known to exist.
            if let Some(prev) = self.files.get_mut(&canon) {
                prev.text = text.to_string();
                prev.parsed_ok = false;
                return;
            }
            self.remove_file_edges(&canon);
            self.files.insert(
                canon.clone(),
                FileIndex {
                    text: text.to_string(),
                    parsed_ok: false,
                    ..FileIndex::default()
                },
            );
            return;
        }

        let dir = canon.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let file = build_file_index(text, &canon, &dir, parsed);

        // Update the global maps: first drop this file's old contributions.
        self.remove_file_from_maps(&canon);

        // Add the new defs to `defs_by_name`.
        for d in &file.defs {
            self.defs_by_name
                .entry(d.name.clone())
                .or_default()
                .push(d.clone());
        }
        // Add the new import edges + reverse edges.
        let edges = file.imports.clone();
        for e in &edges {
            if let Some(target) = &e.resolved {
                self.importers
                    .entry(target.clone())
                    .or_default()
                    .insert(canon.clone());
            }
        }
        self.import_edges.insert(canon.clone(), edges);

        self.files.insert(canon, file);
    }

    /// Remove a file's contributions to `defs_by_name` and `import_edges` /
    /// `importers` (called before re-adding on reindex).
    fn remove_file_from_maps(&mut self, canon: &Path) {
        for defs in self.defs_by_name.values_mut() {
            defs.retain(|d| d.path != canon);
        }
        self.defs_by_name.retain(|_, v| !v.is_empty());
        self.remove_file_edges(canon);
    }

    /// Drop a file's import edges + the reverse-edge entries it contributed.
    fn remove_file_edges(&mut self, canon: &Path) {
        if let Some(old) = self.import_edges.remove(canon) {
            for e in &old {
                if let Some(target) = &e.resolved {
                    if let Some(set) = self.importers.get_mut(target) {
                        set.remove(canon);
                    }
                }
            }
        }
        self.importers.retain(|_, set| !set.is_empty());
    }

    /// The exported-function arity of `(module, name)` for the index-backed
    /// file-module call-arity check (D-arity). Returns `None` when the module is
    /// not indexed, has a parse error, or does not export exactly one fn of that
    /// name with a fixed/derivable arity.
    pub(crate) fn exported_fn_arity(
        &self,
        module: &Path,
        name: &str,
    ) -> Option<crate::check::rules::Arity> {
        let canon = canonicalize(module);
        let file = self.files.get(&canon)?;
        if !file.parsed_ok {
            return None;
        }
        let def = file.exports.get(name)?;
        if def.kind != DefKind::Fn {
            return None;
        }
        // Re-parse the target to read the fn's param list (cheap; cached text).
        let parsed = crate::syntax::parser::parse(&file.text);
        if !parsed.errors.is_empty() {
            return None;
        }
        let tree = crate::syntax::tree_builder::build_tree(parsed);
        let fn_decl = tree.descendants().find(|n| {
            n.kind() == SyntaxKind::FnDecl
                && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
                && ByteSpan::from(name_range_of(n)) == def.name_range
        })?;
        Some(crate::check::rules::decl_arity(fn_decl))
    }
}

/// Lexically canonicalize a path (resolve `.`/`..` components) WITHOUT touching
/// the filesystem, so the index is deterministic and `Send + Sync`-friendly.
fn canonicalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
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

/// Resolve an import specifier to a target file path, mirroring the runtime rule
/// (`Interp::resolve_import`): join against the importer's dir, append `.as` if no
/// extension. `std/*` and bare (non-relative) specifiers resolve to `None`.
fn resolve_specifier(spec: &str, importer_dir: &Path) -> Option<PathBuf> {
    if spec.starts_with("std/") {
        return None;
    }
    // Only relative file imports resolve to a path (mirroring the checker's
    // std-vs-file split). A `./mod` or `../mod` form.
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut p = importer_dir.join(spec);
    if p.extension().is_none() {
        p.set_extension("as");
    }
    Some(canonicalize(&p))
}

/// The NAME-token range of a top-level declaration node (the first `Ident` for a
/// `fn`/`class`/`enum`/`let`/`const`, or the alias/clause for an import). Falls
/// back to the node's full range.
fn name_range_of(node: &crate::syntax::cst::ResolvedNode) -> cstree::text::TextRange {
    node.children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text_range())
        .unwrap_or_else(|| node.text_range())
}

/// Build a `FileIndex` from a freshly-parsed file, projecting the CST + resolver
/// result into defs / exports / uses / import edges.
fn build_file_index(
    text: &str,
    path: &Path,
    dir: &Path,
    parsed: crate::syntax::parser::Parse,
) -> FileIndex {
    use SyntaxKind::*;
    let tree = crate::syntax::tree_builder::build_tree(parsed);
    let resolved = crate::syntax::resolve::resolve(&tree);

    let mut defs: Vec<SymbolDef> = Vec::new();
    let mut exports: HashMap<String, SymbolDef> = HashMap::new();
    let mut imports: Vec<ImportEdge> = Vec::new();

    // The root node IS the `SourceFile`; walk its direct top-level children
    // (unwrapping a leading `export`).
    for child in tree.children() {
        let (decl, is_export): (crate::syntax::cst::ResolvedNode, bool) =
            if child.kind() == ExportStmt {
                match child.children().next() {
                    Some(d) => (d.clone(), true),
                    None => continue,
                }
            } else {
                (child.clone(), false)
            };
        if decl.kind() == ImportStmt {
            imports.push(import_edge(&decl, dir));
            // Imports also bind names — record them as defs so they can be a
            // go-to / rename source within this file.
            for (name, range) in import_names(&decl) {
                defs.push(SymbolDef {
                    name,
                    kind: DefKind::Import,
                    path: path.to_path_buf(),
                    name_range: ByteSpan::from(range),
                });
            }
            continue;
        }
        let Some(kind) = decl_kind(&decl) else {
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(&decl) else {
            continue;
        };
        let name_range = ByteSpan::from(name_range_of(&decl));
        let def = SymbolDef {
            name: name.clone(),
            kind,
            path: path.to_path_buf(),
            name_range,
        };
        defs.push(def.clone());
        if is_export {
            exports.insert(name, def);
        }
    }

    let uses = collect_uses(&tree, &resolved, &defs, &imports, dir);

    FileIndex {
        text: text.to_string(),
        exports,
        defs,
        uses,
        imports,
        parsed_ok: true,
    }
}

/// The [`DefKind`] of a top-level declaration node, or `None` if it binds nothing.
fn decl_kind(decl: &crate::syntax::cst::ResolvedNode) -> Option<DefKind> {
    use SyntaxKind::*;
    Some(match decl.kind() {
        FnDecl => DefKind::Fn,
        ClassDecl => DefKind::Class,
        EnumDecl => DefKind::Enum,
        LetStmt => {
            // `const` vs `let` by the leading keyword token.
            if decl
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == ConstKw)
            {
                DefKind::Const
            } else {
                DefKind::Let
            }
        }
        _ => return None,
    })
}

/// The import-edge descriptor of an `ImportStmt`.
fn import_edge(import: &crate::syntax::cst::ResolvedNode, dir: &Path) -> ImportEdge {
    let specifier = import_specifier(import).unwrap_or_default();
    let resolved = resolve_specifier(&specifier, dir);
    let names = import_names(import).into_iter().map(|(n, _)| n).collect();
    ImportEdge {
        specifier,
        resolved,
        names,
    }
}

/// The `from "<spec>"` string of an `ImportStmt`, quote-stripped.
fn import_specifier(import: &crate::syntax::cst::ResolvedNode) -> Option<String> {
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

/// The imported `(name, name_range)` pairs of an `ImportStmt` (named-list members,
/// or the namespace alias).
fn import_names(
    import: &crate::syntax::cst::ResolvedNode,
) -> Vec<(String, cstree::text::TextRange)> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    if let Some(list) = import.children().find(|c| c.kind() == ImportList) {
        for t in list.children_with_tokens().filter_map(|el| el.into_token()) {
            if t.kind() == Ident {
                out.push((t.text().to_string(), t.text_range()));
            }
        }
    } else {
        // Namespace `import * as <alias>`: the alias is the Ident after `as`.
        let idents: Vec<_> = import
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident)
            .collect();
        if let Some(pos) = idents.iter().position(|t| t.text() == "as") {
            if let Some(alias) = idents.get(pos + 1) {
                out.push((alias.text().to_string(), alias.text_range()));
            }
        }
    }
    out
}

/// Project the resolver's `uses` into [`UseSite`]s with cross-file targets. A use
/// of an imported name is linked to its module's path via the import edge.
fn collect_uses(
    tree: &crate::syntax::cst::ResolvedNode,
    resolved: &crate::syntax::resolve::types::ResolveResult,
    defs: &[SymbolDef],
    imports: &[ImportEdge],
    _dir: &Path,
) -> Vec<UseSite> {
    use crate::syntax::resolve::types::Resolution;
    use SyntaxKind::*;
    // Map an imported NAME -> its resolved module path (first import wins).
    let mut import_module: HashMap<String, Option<PathBuf>> = HashMap::new();
    for e in imports {
        for n in &e.names {
            import_module
                .entry(n.clone())
                .or_insert_with(|| e.resolved.clone());
        }
    }
    // Map a local def NAME -> its name_range (for same-file LocalDef targets).
    let mut local_def: HashMap<&str, ByteSpan> = HashMap::new();
    for d in defs {
        local_def.entry(d.name.as_str()).or_insert(d.name_range);
    }

    let mut out = Vec::new();
    for nameref in tree.descendants().filter(|n| n.kind() == NameRef) {
        let Some(name) = crate::syntax::resolve::ident_text(nameref) else {
            continue;
        };
        let range = ByteSpan::from(nameref.text_range());
        let target = match resolved.uses.get(&nameref.text_range()) {
            // A use that resolves to an imported name → cross-file target.
            _ if import_module.contains_key(&name) => ResolvedTarget::Imported {
                module: import_module.get(&name).cloned().flatten(),
                name: name.clone(),
            },
            // A use that resolves to a file-local global → same-file def.
            Some(Resolution::Global(_)) | Some(Resolution::Local(_) | Resolution::Upvalue(_)) => {
                match local_def.get(name.as_str()) {
                    Some(span) => ResolvedTarget::LocalDef(*span),
                    None => ResolvedTarget::Other,
                }
            }
            _ => ResolvedTarget::Other,
        };
        out.push(UseSite {
            range,
            name,
            target,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert (at compile time) that the index is `Send + Sync` — the LSP layer
    /// must never hold a non-`Send` interpreter type.
    #[allow(dead_code)]
    fn assert_send_sync<T: Send + Sync>() {}
    #[test]
    fn index_is_send_sync() {
        assert_send_sync::<WorkspaceIndex>();
        assert_send_sync::<FileIndex>();
        assert_send_sync::<SymbolDef>();
    }

    fn fixture() -> WorkspaceIndex {
        let a = (
            PathBuf::from("/ws/a.as"),
            "export fn f(x) { return x }\nlet helper = 1\n".to_string(),
        );
        let b = (
            PathBuf::from("/ws/b.as"),
            "import { f } from \"./a\"\nprint(f(1))\n".to_string(),
        );
        let c = (
            PathBuf::from("/ws/c.as"),
            "let unrelated = 2\nprint(unrelated)\n".to_string(),
        );
        WorkspaceIndex::build_from_files(&[a, b, c])
    }

    #[test]
    fn defs_by_name_collects_across_files() {
        let idx = fixture();
        // `f` is DEFINED in a.as (a fn) and also bound (as an import) in b.as.
        let fs = &idx.defs_by_name["f"];
        let fn_def = fs
            .iter()
            .find(|d| d.kind == DefKind::Fn)
            .expect("a fn def for f");
        assert_eq!(fn_def.path, PathBuf::from("/ws/a.as"));
        assert!(fs.iter().any(|d| d.kind == DefKind::Import && d.path == PathBuf::from("/ws/b.as")));
        assert!(idx.defs_by_name.contains_key("unrelated"));
    }

    #[test]
    fn exports_recorded_for_defining_file() {
        let idx = fixture();
        let a = &idx.files[&PathBuf::from("/ws/a.as")];
        assert!(a.exports.contains_key("f"), "a should export f");
        // `helper` is NOT exported.
        assert!(!a.exports.contains_key("helper"));
    }

    #[test]
    fn import_edges_and_importers_correct() {
        let idx = fixture();
        let b = PathBuf::from("/ws/b.as");
        let a = PathBuf::from("/ws/a.as");
        let edges = &idx.import_edges[&b];
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].resolved.as_ref(), Some(&a));
        assert_eq!(edges[0].names, vec!["f".to_string()]);
        // Reverse: a is imported by b.
        assert!(idx.importers[&a].contains(&b));
    }

    #[test]
    fn b_use_of_f_targets_a() {
        let idx = fixture();
        let b = &idx.files[&PathBuf::from("/ws/b.as")];
        let use_f = b
            .uses
            .iter()
            .find(|u| u.name == "f")
            .expect("b should use f");
        assert_eq!(
            use_f.target,
            ResolvedTarget::Imported {
                module: Some(PathBuf::from("/ws/a.as")),
                name: "f".to_string(),
            }
        );
    }

    #[test]
    fn editing_c_leaves_a_and_b_untouched() {
        let mut idx = fixture();
        let a_before = idx.files[&PathBuf::from("/ws/a.as")].clone();
        let b_before = idx.files[&PathBuf::from("/ws/b.as")].clone();
        idx.reindex_file(
            &PathBuf::from("/ws/c.as"),
            "let unrelated = 99\nprint(unrelated)\n",
        );
        assert_eq!(idx.files[&PathBuf::from("/ws/a.as")], a_before);
        assert_eq!(idx.files[&PathBuf::from("/ws/b.as")], b_before);
    }

    #[test]
    fn parse_error_retains_last_good_index() {
        let mut idx = fixture();
        let b = PathBuf::from("/ws/b.as");
        let good = idx.files[&b].clone();
        // Type a syntax error into b.
        idx.reindex_file(&b, "import { f } from \"./a\"\nprint(f(@@@\n");
        let now = &idx.files[&b];
        assert!(!now.parsed_ok, "b should be marked not-ok");
        // The last-good defs/uses survive (navigation still works).
        assert_eq!(now.uses, good.uses);
        assert_eq!(now.imports, good.imports);
    }

    #[test]
    fn exported_fn_arity_reads_target_signature() {
        let idx = fixture();
        let a = PathBuf::from("/ws/a.as");
        let arity = idx.exported_fn_arity(&a, "f").expect("f has arity");
        assert_eq!(arity.min, 1);
        assert_eq!(arity.max, Some(1));
        // A non-exported / unknown name → None.
        assert!(idx.exported_fn_arity(&a, "helper").is_none());
        assert!(idx.exported_fn_arity(&a, "nope").is_none());
    }
}
