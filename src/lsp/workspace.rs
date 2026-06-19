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
///
/// This is FRAME-PRECISE (DX D3 Task 11): a `LocalDecl` use carries the byte range
/// of the binding's DECLARATION as resolved by the per-file `syntax::resolve` frame
/// walk — NOT a name-coarse match — so two same-named sibling locals / a shadow of
/// an imported name resolve to distinct targets. The cross-file identity is then
/// lifted by pairing this with the file's [`FileId`] (see [`GlobalBindingId`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// A `Local`/`Upvalue` use: the byte range of its DECL in the SAME file
    /// (frame-precise — the exact shadowing binding, not the first by name).
    LocalDecl(ByteSpan),
    /// A MODULE-GLOBAL use whose definer is THIS file: the def's name-range.
    GlobalDef(ByteSpan),
    /// An imported name: `module` is the resolved file path (or `None` for std),
    /// `name` the imported symbol. Lifts THROUGH the import edge to the definer.
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

/// One parameter of an exported function, extracted from the CST for signature help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedParam {
    pub name: String,
    pub ty: Option<String>,
    pub optional: bool,
    pub variadic: bool,
}

/// The parameter signature of an exported function, for cross-file signature help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedFnSig {
    pub params: Vec<ExportedParam>,
    pub ret: Option<String>,
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
    /// DX D3 Task 11: every NON-global (frame-local) binding's `(name-token range,
    /// frame-precise decl range)`. Lets `binding_id_at` resolve a cursor sitting on
    /// a LOCAL declaration (e.g. a `let x` inside a function) to its file-qualified
    /// `GlobalBindingId::Local(thisFile, decl_range)` — the in-file frame-precise
    /// projection, lifted by FileId.
    pub local_decls: Vec<(ByteSpan, ByteSpan)>,
    /// `true` if the file parsed without a syntax error. On a parse error the
    /// previous (last-good) `FileIndex` is retained, so this is only `false` for a
    /// freshly-added file that never parsed.
    pub parsed_ok: bool,
}

/// A stable, file-qualified identity for the workspace (DX D3 Task 11). Interned
/// over the index's canonical `PathBuf` keys; stable for the index's lifetime. The
/// disambiguator that makes two same-named, same-byte-range locals in DIFFERENT
/// files distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// The UNIFIED binding identity shared by in-file and cross-file navigation
/// (DX D3 Task 11, spec §4.1). The single model that replaces the old divergence
/// between frame-precise in-file `navigation::BindingId` and name-coarse cross-file
/// matching:
///
/// - a `Local`/`Upvalue` use lifts to `Local(use's FileId, its DECL TextRange)` —
///   frame-precise AND file-qualified (so a same-range local in another file is a
///   DISTINCT identity);
/// - a module-global / exported name lifts to `Global(definer FileId, name)`. An
///   importer's use of an imported name resolves THROUGH its `ImportEdge` to the
///   DEFINER's `FileId`, so every importer's use + the def collapse to ONE
///   `Global(definerFileId, name)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlobalBindingId {
    /// A frame-precise local/upvalue: the use's file + its decl's byte range.
    Local(FileId, ByteSpan),
    /// A module-global / export: the DEFINER's file + the exported name.
    Global(FileId, String),
}

/// A `PathBuf <-> FileId` interner (an additive side-table; the `files` map stays
/// keyed by `PathBuf`). Ids are assigned densely and never reused for the index's
/// lifetime, so a `FileId` is a stable handle.
#[derive(Debug, Clone, Default)]
pub struct FileInterner {
    /// `FileId(i)` -> the canonical path at index `i`.
    paths: Vec<PathBuf>,
    /// Reverse: canonical path -> its `FileId`.
    ids: HashMap<PathBuf, FileId>,
}

impl FileInterner {
    /// Intern `canon` (already canonical), returning its stable [`FileId`].
    fn intern(&mut self, canon: &Path) -> FileId {
        if let Some(id) = self.ids.get(canon) {
            return *id;
        }
        let id = FileId(self.paths.len() as u32);
        self.paths.push(canon.to_path_buf());
        self.ids.insert(canon.to_path_buf(), id);
        id
    }

    /// The [`FileId`] of `canon` if it has been interned.
    fn get(&self, canon: &Path) -> Option<FileId> {
        self.ids.get(canon).copied()
    }

    /// The canonical path of `id`.
    fn path(&self, id: FileId) -> Option<&Path> {
        self.paths.get(id.0 as usize).map(|p| p.as_path())
    }
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
    /// DX D3 Task 11: the `PathBuf <-> FileId` interner backing the unified
    /// [`GlobalBindingId`]. An additive side-table over the canonical `files` keys.
    interner: FileInterner,
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

    // ---- DX D3 Task 11: file-qualified identity ---------------------------

    /// The stable [`FileId`] of `path` (canonicalized first), if it is indexed.
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.interner.get(&canonicalize(path))
    }

    /// The canonical path of a [`FileId`].
    pub fn path_of(&self, id: FileId) -> Option<&Path> {
        self.interner.path(id)
    }

    /// The UNIFIED [`GlobalBindingId`] the cursor at byte `offset` in `path`
    /// refers to — the single identity both in-file and cross-file navigation
    /// share (DX D3 Task 11). The cursor may sit on a definition, a same-file
    /// frame-precise local/upvalue use, a module-global use, or an imported use.
    ///
    /// The lift:
    /// - a `Local`/`Upvalue` use → `Local(this file's FileId, its DECL range)`
    ///   (frame-precise — the exact shadowing binding);
    /// - a module-global def/use in THIS file → `Global(this FileId, name)`;
    /// - an imported use → resolve the [`ImportEdge`] to the DEFINER file and
    ///   return `Global(definer FileId, name)`, so every importer's use and the
    ///   def collapse to ONE identity.
    pub fn binding_id_at(&self, path: &Path, offset: usize) -> Option<GlobalBindingId> {
        let canon = canonicalize(path);
        let file = self.files.get(&canon)?;
        let this_id = self.interner.get(&canon)?;

        // 1) On a top-level definition's own name token?
        for d in &file.defs {
            if offset >= d.name_range.start && offset < d.name_range.end {
                return self.def_identity(&canon, this_id, d);
            }
        }
        // 2) On a frame-LOCAL declaration's name token? → file-qualified Local.
        for (name_tok, decl) in &file.local_decls {
            if offset >= name_tok.start && offset < name_tok.end {
                return Some(GlobalBindingId::Local(this_id, *decl));
            }
        }
        // 3) On a use? Lift its frame-precise/cross-file target.
        let site = file
            .uses
            .iter()
            .find(|u| offset >= u.range.start && offset < u.range.end)?;
        self.use_identity(&canon, this_id, site)
    }

    /// The identity of a DEFINITION site in `canon` (FileId `this_id`).
    fn def_identity(
        &self,
        canon: &Path,
        this_id: FileId,
        d: &SymbolDef,
    ) -> Option<GlobalBindingId> {
        if d.kind == DefKind::Import {
            // An import binding's identity IS the definer's Global identity. Resolve
            // it through the import EDGE, not `definition_at`: the import-clause name
            // is NOT emitted as a `UseSite` (it is not a `NameRef`), so `definition_at`
            // would return `None` and the identity would wrongly fall back to THIS
            // (the importer's) FileId — making references/rename from a cursor ON the
            // import clause find nothing / produce a corrupt partial rename.
            if let Some(file) = self.files.get(canon) {
                for e in &file.imports {
                    if e.names.iter().any(|n| n == &d.name) {
                        if let Some(module) = &e.resolved {
                            if let Some(def_id) = self.interner.get(module) {
                                return Some(GlobalBindingId::Global(def_id, d.name.clone()));
                            }
                        }
                    }
                }
            }
            // An unresolved/std import (or a not-yet-indexed target): fall back to a
            // this-file Global so the import clause is at least self-consistent.
            return Some(GlobalBindingId::Global(this_id, d.name.clone()));
        }
        // A top-level decl is a module-global → Global(thisFile, name).
        Some(GlobalBindingId::Global(this_id, d.name.clone()))
    }

    /// The identity of a USE site in `canon` (FileId `this_id`).
    fn use_identity(
        &self,
        canon: &Path,
        this_id: FileId,
        site: &UseSite,
    ) -> Option<GlobalBindingId> {
        match &site.target {
            // Frame-precise local/upvalue → file-qualified by THIS file.
            ResolvedTarget::LocalDecl(decl) => Some(GlobalBindingId::Local(this_id, *decl)),
            // A module-global whose definer is this file.
            ResolvedTarget::GlobalDef(_) => {
                Some(GlobalBindingId::Global(this_id, site.name.clone()))
            }
            // An imported name → lift THROUGH the edge to the definer's FileId.
            ResolvedTarget::Imported {
                module: Some(module),
                name,
            } => {
                let def_id = self.interner.get(module)?;
                Some(GlobalBindingId::Global(def_id, name.clone()))
            }
            // A std import or otherwise unresolved use has no workspace identity.
            ResolvedTarget::Imported { module: None, .. } | ResolvedTarget::Other => {
                let _ = canon;
                None
            }
        }
    }

    /// Re-index a single file (incremental update on `didOpen`/`didChange`). On a
    /// parse error the file's LAST-GOOD index is retained (navigation degrades
    /// gracefully); a never-parsed file gets an empty `parsed_ok = false` entry.
    /// Updates the `defs_by_name` / `import_edges` / `importers` deltas for this
    /// file only — a cross-file use target is resolved lazily at query time via the
    /// import edge, so editing one file never invalidates another's own index.
    pub fn reindex_file(&mut self, path: &Path, text: &str) {
        let canon = canonicalize(path);
        // Intern the file so it has a stable FileId for the unified identity, even
        // if it never parses cleanly (a known-to-exist file is still file-qualified).
        self.interner.intern(&canon);
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

    /// Fully remove a path from the index: its `defs_by_name` / `import_edges` /
    /// `importers` contributions AND its `files` entry. Use for a path that is
    /// genuinely going away (rename / delete) — this makes forgetting the second
    /// half impossible. `reindex_file` uses the lower-level
    /// `remove_file_from_maps` instead because it re-inserts immediately after.
    pub fn fully_unindex(&mut self, canon: &Path) {
        self.remove_file_from_maps(canon);
        self.files.remove(canon);
    }

    /// Remove a file's contributions to `defs_by_name` and `import_edges` /
    /// `importers` (called before re-adding on reindex).
    ///
    /// This does NOT remove the file's own `files` entry — a caller that fully
    /// drops a path must use [`Self::fully_unindex`] instead. `reindex_file`
    /// calls this then re-inserts.
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

    /// The exported-function parameter signature for `(module, name)`, for
    /// signature-help cross-file resolution (SIG §3.1 rung c). Returns `None`
    /// when the module is not indexed, has parse errors, or does not export
    /// exactly one fn of that name.
    pub(crate) fn exported_fn_signature(
        &self,
        module: &Path,
        name: &str,
    ) -> Option<ExportedFnSig> {
        let canon = canonicalize(module);
        let file = self.files.get(&canon)?;
        if !file.parsed_ok {
            return None;
        }
        let def = file.exports.get(name)?;
        if def.kind != DefKind::Fn {
            return None;
        }
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
        Some(exported_fn_sig_from_decl(fn_decl))
    }

    /// Signature-help variant: look up a cross-file fn by tracing the import edge
    /// from the current document. Returns `None` when the import is ambiguous,
    /// unresolved, or the module is not indexed.
    pub(crate) fn exported_fn_signature_by_import(
        &self,
        model: &crate::lsp::model::SemanticModel,
        doc_path: &Path,
        name: &str,
    ) -> Option<ExportedFnSig> {
        let canon = canonicalize(doc_path);
        let file = self.files.get(&canon)?;
        let mut count = 0usize;
        let mut target: Option<&std::path::PathBuf> = None;
        for e in &file.imports {
            if let Some(resolved) = &e.resolved {
                for n in &e.names {
                    if n == name {
                        count += 1;
                        target = Some(resolved);
                    }
                }
            }
        }
        // Std/* imports are handled by the stdlib rung, not here.
        if count != 1 {
            return None;
        }
        let _ = model;
        self.exported_fn_signature(target?, name)
    }

    // ---- D-arity: index-backed file-module call-arity ---------------------

    /// Extra `call-arity` diagnostics for calls to IMPORTED FILE-module functions
    /// in `path` (D-arity). The path-less `crate::check::analyze` cannot see across
    /// files; this uses the index's `exported_fn_arity` to flag a call to a
    /// uniquely-imported file fn with too few / too many args. Zero-FP: only flags
    /// when the import resolves to an indexed, cleanly-parsed module that exports
    /// exactly one fn of that name with a derivable arity; skips spreads and any
    /// uncertainty. Returns diagnostics over `text` (byte ranges).
    pub fn file_module_arity(&self, path: &Path, text: &str) -> Vec<crate::check::AsDiagnostic> {
        use crate::check::diagnostic::{AsDiagnostic, Severity};
        use SyntaxKind::*;
        let canon = canonicalize(path);
        let Some(file) = self.files.get(&canon) else {
            return Vec::new();
        };
        if !file.parsed_ok {
            return Vec::new();
        }
        // Map each uniquely-imported name -> its resolved FILE-module path.
        let mut import_count: HashMap<&str, usize> = HashMap::new();
        let mut import_path: HashMap<&str, &PathBuf> = HashMap::new();
        for e in &file.imports {
            if let Some(target) = &e.resolved {
                for n in &e.names {
                    *import_count.entry(n.as_str()).or_default() += 1;
                    import_path.insert(n.as_str(), target);
                }
            }
        }

        let parsed = crate::syntax::parser::parse(text);
        if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() {
            return Vec::new();
        }
        let tree = crate::syntax::tree_builder::build_tree(parsed);
        let mut out = Vec::new();
        for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
            let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
                continue;
            };
            let Some(name) = crate::syntax::resolve::ident_text(callee) else {
                continue;
            };
            if import_count.get(name.as_str()).copied() != Some(1) {
                continue;
            }
            let Some(module) = import_path.get(name.as_str()) else {
                continue;
            };
            let Some(arity) = self.exported_fn_arity(module, &name) else {
                continue; // not an exported fn / parse error / ambiguous → skip
            };
            let Some(arg_list) = call.children().find(|c| c.kind() == ArgList) else {
                continue;
            };
            if arg_list.children().any(|c| c.kind() == SpreadElem) {
                continue; // spread → unknown count
            }
            let argc = arg_list
                .children()
                .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
                .count();
            let too_few = argc < arity.min;
            let too_many = arity.max.is_some_and(|m| argc > m);
            if too_few || too_many {
                let expected = match arity.max {
                    Some(m) if m == arity.min => format!("{} argument(s)", arity.min),
                    Some(m) => format!("{} to {m} argument(s)", arity.min),
                    None => format!("at least {} argument(s)", arity.min),
                };
                out.push(AsDiagnostic {
                    range: crate::check::rules::code_range(call),
                    severity: Severity::Warning,
                    code: "call-arity".to_string(),
                    message: format!("{name} expects {expected} but is called with {argc}"),
                    fix: None,
                });
            }
        }
        out
    }

    // ---- L2: cross-file go-to-definition ----------------------------------

    /// Resolve the use at byte `offset` in `path` to a defining `(path, range)`.
    /// Cross-file when the use is an imported name (links via the import edge to
    /// the target file's export); same-file otherwise. `None` if no use is at the
    /// cursor or it does not resolve to a known def.
    pub fn definition_at(&self, path: &Path, offset: usize) -> Option<(PathBuf, ByteSpan)> {
        let canon = canonicalize(path);
        let file = self.files.get(&canon)?;
        let site = file
            .uses
            .iter()
            .find(|u| offset >= u.range.start && offset < u.range.end)?;
        match &site.target {
            ResolvedTarget::LocalDecl(span) | ResolvedTarget::GlobalDef(span) => {
                Some((canon, *span))
            }
            ResolvedTarget::Imported {
                module: Some(module),
                name,
            } => {
                let target = self.files.get(module)?;
                let def = target.exports.get(name)?;
                Some((module.clone(), def.name_range))
            }
            _ => None,
        }
    }

    // ---- L3: workspace symbols + find-references ---------------------------

    /// Every workspace symbol whose name contains `query` (case-insensitive), as
    /// `(def)`. An empty query returns all defs. Skips `Import` re-bindings (they
    /// are not new symbols).
    pub fn workspace_symbols(&self, query: &str) -> Vec<SymbolDef> {
        let q = query.to_lowercase();
        let mut out: Vec<SymbolDef> = Vec::new();
        for defs in self.defs_by_name.values() {
            for d in defs {
                if d.kind == DefKind::Import {
                    continue;
                }
                if q.is_empty() || d.name.to_lowercase().contains(&q) {
                    out.push(d.clone());
                }
            }
        }
        // Deterministic order: by name then path.
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
        out
    }

    /// Find all references to the definition at byte `offset` in `path`: the def's
    /// own name range + every `UseSite` targeting it across the def's file and its
    /// importers. `include_decl` controls whether the declaration's own name range
    /// is included. Returns `(path, range)` locations.
    pub fn references_at(
        &self,
        path: &Path,
        offset: usize,
        include_decl: bool,
    ) -> Vec<(PathBuf, ByteSpan)> {
        // DX D3 Task 11: resolve the cursor to the UNIFIED identity, then collect
        // every use (and the decl) whose lifted `GlobalBindingId` EQUALS it. This is
        // the frame-precise join — a use is a reference iff it shares the identity,
        // NOT iff it shares the name (so a shadowing local of an imported name is
        // correctly excluded).
        let canon = canonicalize(path);
        let Some(target_id) = self.binding_id_at(&canon, offset) else {
            return Vec::new();
        };
        let mut out: Vec<(PathBuf, ByteSpan)> = Vec::new();

        // The decl site + the set of files to scan, derived from the identity.
        let resolved: Option<(PathBuf, ByteSpan, Vec<PathBuf>)> = match &target_id {
            GlobalBindingId::Local(fid, decl) => {
                // A frame-precise local: the decl + uses live in ONE file only. The
                // decl EDIT site is the binding's NAME token (from `local_decls`), not
                // the whole decl-node range that keys the identity — so a rename
                // replaces just `x`, not the entire `let x = …` statement.
                self.interner.path(*fid).map(|p| {
                    let p = p.to_path_buf();
                    let name_range = self
                        .files
                        .get(&p)
                        .and_then(|f| f.local_decls.iter().find(|(_, d)| d == decl))
                        .map(|(name_tok, _)| *name_tok)
                        .unwrap_or(*decl);
                    (p.clone(), name_range, vec![p])
                })
            }
            GlobalBindingId::Global(fid, name) => self.interner.path(*fid).and_then(|p| {
                let p = p.to_path_buf();
                // The definer's name-range for the decl edit.
                let range = self
                    .files
                    .get(&p)
                    .and_then(|f| {
                        f.defs
                            .iter()
                            .find(|d| d.name == *name && d.kind != DefKind::Import)
                    })
                    .map(|d| d.name_range)?;
                // Scan the definer + every importer of it.
                let mut scan = vec![p.clone()];
                if let Some(importers) = self.importers.get(&p) {
                    scan.extend(importers.iter().cloned());
                }
                Some((p, range, scan))
            }),
        };
        let Some((def_path, def_range, scan)) = resolved else {
            return out;
        };

        if include_decl {
            out.push((def_path.clone(), def_range));
        }
        for file_path in scan {
            let Some(file) = self.files.get(&file_path) else {
                continue;
            };
            let Some(this_id) = self.interner.get(&file_path) else {
                continue;
            };
            for u in &file.uses {
                if self.use_identity(&file_path, this_id, u).as_ref() == Some(&target_id) {
                    out.push((file_path.clone(), u.range));
                }
            }
        }
        out
    }

    /// Resolve the cursor to the CANONICAL definition it refers to (following an
    /// import to the defining file), as `(def_path, name, name_range)`. The cursor
    /// may be on the definition itself, a same-file use, or an imported use.
    pub fn def_at(&self, path: &Path, offset: usize) -> Option<(PathBuf, String, ByteSpan)> {
        let canon = canonicalize(path);
        let file = self.files.get(&canon)?;
        // On a definition's own name?
        for d in &file.defs {
            if offset >= d.name_range.start && offset < d.name_range.end {
                // If it's an import binding, follow it to the real def.
                if d.kind == DefKind::Import {
                    if let Some((m, span)) = self.definition_at(&canon, offset) {
                        return Some((m, d.name.clone(), span));
                    }
                }
                return Some((canon, d.name.clone(), d.name_range));
            }
        }
        // On a use? Resolve it to the def.
        let (def_path, def_range) = self.definition_at(&canon, offset)?;
        let name = file
            .uses
            .iter()
            .find(|u| offset >= u.range.start && offset < u.range.end)
            .map(|u| u.name.clone())?;
        Some((def_path, name, def_range))
    }

    // ---- L4: rename across files ------------------------------------------

    /// Whether the definition at `offset` in `path` is renameable: it must resolve
    /// to a known def AND every touched file (the def's file + its importers) must
    /// have parsed cleanly. Returns the def's current name + name range for a
    /// `prepareRename`.
    pub fn prepare_rename(&self, path: &Path, offset: usize) -> Option<(String, ByteSpan)> {
        // DX D3 Task 12: prepareRename accepts any renameable binding the unified
        // identity resolves — a top-level def/export, an imported use, OR a
        // frame-local decl/use — refusing only when the binding has no identity or a
        // touched file has a parse error (so a local rename is no longer rejected).
        let canon = canonicalize(path);
        let id = self.binding_id_at(&canon, offset)?;
        let scope_root = match &id {
            GlobalBindingId::Local(fid, _) | GlobalBindingId::Global(fid, _) => {
                self.interner.path(*fid)?.to_path_buf()
            }
        };
        if !self.rename_scope_is_clean(&scope_root) {
            return None;
        }
        // The token under the cursor in THIS file (for the prepare highlight + the
        // current name): a top-level def name, a frame-local decl name, or a use.
        let file = self.files.get(&canon)?;
        if let Some(d) = file
            .defs
            .iter()
            .find(|d| offset >= d.name_range.start && offset < d.name_range.end)
        {
            return Some((d.name.clone(), d.name_range));
        }
        if let Some((name_tok, _)) = file
            .local_decls
            .iter()
            .find(|(name_tok, _)| offset >= name_tok.start && offset < name_tok.end)
        {
            return Some((slice_text(&file.text, *name_tok), *name_tok));
        }
        file.uses
            .iter()
            .find(|u| offset >= u.range.start && offset < u.range.end)
            .map(|u| (u.name.clone(), u.range))
    }

    /// Build the rename edit set: every reference to the def at `offset` (decl +
    /// import clauses + use sites) across the def's file and its direct importers,
    /// as `(path, range)` to replace with `new_name`. Returns `None` (refuse) if
    /// the position is not renameable, the new name collides with an existing
    /// top-level def in a touched file, or any touched file has a parse error.
    pub fn rename_edits(
        &self,
        path: &Path,
        offset: usize,
        new_name: &str,
    ) -> Option<Vec<(PathBuf, ByteSpan)>> {
        // DX D3 Task 12: route rename through the UNIFIED identity (the same
        // `binding_id_at`/`references_at` join references uses), so renaming a
        // FRAME-LOCAL binding works and the Task-11 shadowing edge holds end-to-end
        // (a shadowing local of an imported name is never swept into the export's
        // rename, and a local rename never escapes its file).
        let canon = canonicalize(path);
        let id = self.binding_id_at(&canon, offset)?;
        // The scope root is the binding's home file: for a Local it is the only file
        // touched; for a Global it is the DEFINER (its importers are touched too).
        let (scope_root, global_name) = match &id {
            GlobalBindingId::Local(fid, _) => (self.interner.path(*fid)?.to_path_buf(), None),
            GlobalBindingId::Global(fid, name) => {
                (self.interner.path(*fid)?.to_path_buf(), Some(name.clone()))
            }
        };
        if !self.rename_scope_is_clean(&scope_root) {
            return None;
        }
        // Collision guard: the new name must not already be a top-level def in the
        // home file or — for a global — any importer.
        let mut touched: Vec<PathBuf> = vec![scope_root.clone()];
        if global_name.is_some() {
            if let Some(importers) = self.importers.get(&scope_root) {
                touched.extend(importers.iter().cloned());
            }
        }
        for fp in &touched {
            if let Some(file) = self.files.get(fp) {
                if file.defs.iter().any(|d| d.name == new_name) {
                    return None; // collision in a touched scope
                }
            }
        }
        // Collect the decl + every reference by the unified identity (works for both
        // a Local — one file — and a Global — definer + importers).
        let mut edits = self.references_at(&canon, offset, true);
        // For a global/export, also rename the import-clause name tokens in importers
        // (they are `defs` of kind Import in those files with the same name).
        if let Some(name) = global_name {
            if let Some(importers) = self.importers.get(&scope_root) {
                for imp in importers {
                    if let Some(file) = self.files.get(imp) {
                        for d in &file.defs {
                            if d.kind == DefKind::Import && d.name == name {
                                let loc = (imp.clone(), d.name_range);
                                if !edits.contains(&loc) {
                                    edits.push(loc);
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(edits)
    }

    /// Every file touched by a rename of a def in `def_path` (the file + its
    /// importers) must have parsed cleanly, else the edit would be unsafe.
    fn rename_scope_is_clean(&self, def_path: &Path) -> bool {
        let ok = |p: &Path| self.files.get(p).map(|f| f.parsed_ok).unwrap_or(false);
        if !ok(def_path) {
            return false;
        }
        if let Some(importers) = self.importers.get(def_path) {
            importers.iter().all(|p| ok(p))
        } else {
            true
        }
    }

    /// Compute the import-specifier rewrites needed when `old_path` is renamed to
    /// `new_path`: for every file that imports `old_path`, the byte range of its
    /// `from "<spec>"` string token's INNER text (between the quotes) + the NEW
    /// importer-relative specifier. Returns `(importer_path, specifier_range,
    /// new_specifier)`. The new specifier is the importer-relative path to
    /// `new_path` WITHOUT the `.as` extension and WITH a leading `./` (mirroring
    /// the forms `resolve_specifier` accepts).
    pub fn import_rewrite_edits(
        &self,
        old_path: &Path,
        new_path: &Path,
    ) -> Vec<(PathBuf, ByteSpan, String)> {
        let old = canonicalize(old_path);
        let mut out = Vec::new();
        let Some(importers) = self.importers.get(&old) else {
            return out;
        };
        for imp in importers {
            let Some(file) = self.files.get(imp) else {
                continue;
            };
            let importer_dir = imp.parent().map(Path::to_path_buf).unwrap_or_default();
            let new_spec = relative_specifier(&importer_dir, new_path);
            // Re-parse the importer to find the import statement whose resolved
            // target is `old`, and the byte range of its `from "<spec>"` STRING.
            let parsed = crate::syntax::parser::parse(&file.text);
            if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() {
                continue;
            }
            let tree = crate::syntax::tree_builder::build_tree(parsed);
            for import in tree
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::ImportStmt)
            {
                let Some(spec) = import_specifier(import) else {
                    continue;
                };
                if resolve_specifier(&spec, &importer_dir).as_deref() != Some(old.as_path()) {
                    continue;
                }
                // The string TOKEN range, INNER (between the quotes).
                if let Some(tok) = import
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| t.kind() == SyntaxKind::Str)
                {
                    let r = ByteSpan::from(tok.text_range());
                    let inner = ByteSpan {
                        start: r.start + 1,
                        end: r.end - 1,
                    };
                    out.push((imp.clone(), inner, new_spec.clone()));
                }
            }
        }
        out
    }
}

/// The importer-relative specifier for `target` (a leading `./`/`../`, no `.as`).
fn relative_specifier(importer_dir: &Path, target: &Path) -> String {
    let target = canonicalize(target);
    let target_noext = {
        let mut t = target.clone();
        if t.extension().and_then(|e| e.to_str()) == Some("as") {
            t.set_extension("");
        }
        t
    };
    let rel = pathdiff_lexical(importer_dir, &target_noext);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.starts_with("./") || s.starts_with("../") {
        s
    } else {
        format!("./{s}")
    }
}

/// A lexical relative path from `base` to `target` (no fs access), enough for
/// sibling/`../` import rewrites.
fn pathdiff_lexical(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let targ: Vec<_> = target.components().collect();
    let common = base.iter().zip(&targ).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for c in &targ[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// The text of a byte-`ByteSpan` slice of `src` (the source the span indexes). The
/// span comes from the file's own resolver facts, so it is always on char
/// boundaries; an out-of-range/non-boundary span degrades to `""` rather than
/// panicking.
fn slice_text(src: &str, span: ByteSpan) -> String {
    src.get(span.start..span.end).unwrap_or("").to_string()
}

/// Convert a [`ByteSpan`] into an LSP `Range` against `text` (byte→char→UTF-16
/// position). Shared by the LSP providers.
pub fn byte_span_to_range(text: &str, span: ByteSpan) -> tower_lsp::lsp_types::Range {
    let index = crate::lsp::line_index::LineIndex::new(text);
    let start = index.position(byte_to_char(text, span.start));
    let end = index.position(byte_to_char(text, span.end));
    tower_lsp::lsp_types::Range { start, end }
}

/// Byte offset → char offset (clamped to a char boundary), mirroring
/// `convert::byte_to_char`.
fn byte_to_char(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].chars().count()
}

/// The canonical (lexical) form of a path — public so the server keys the index
/// the same way the index does.
pub fn canon(path: &Path) -> PathBuf {
    canonicalize(path)
}

/// Recursively discover `*.as` files under `root` (depth-bounded, skipping
/// hidden/`target` dirs). Used to warm the index from a workspace folder.
pub fn discover_as_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    discover_into(root, 0, &mut out);
    out
}

fn discover_into(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 32 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            discover_into(&path, depth + 1, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("as") {
            out.push(path);
        }
    }
}

/// Build an `ExportedFnSig` from a `FnDecl` CST node by reading its `ParamList`.
fn exported_fn_sig_from_decl(fn_decl: &crate::syntax::cst::ResolvedNode) -> ExportedFnSig {
    use crate::check::rules::is_type_kind;
    use SyntaxKind::*;
    let param_list = fn_decl.children().find(|c| c.kind() == ParamList);
    let mut params = Vec::new();
    if let Some(list) = param_list {
        for p in list.children().filter(|c| c.kind() == Param) {
            let variadic = p
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot);
            let Some(name) = crate::syntax::resolve::ident_text(p) else {
                continue;
            };
            let optional = p
                .children()
                .any(|c| crate::check::rules::is_expr_kind(c.kind()));
            let ty = p
                .children()
                .find(|c| is_type_kind(c.kind()))
                .map(|t| t.text().to_string().trim().to_string());
            params.push(ExportedParam { name, ty, optional, variadic });
        }
    }
    // Attempt to extract the return-type annotation (the last type child of fn_decl
    // that is NOT inside the ParamList or body block).
    let ret = fn_decl
        .children()
        .filter(|c| is_type_kind(c.kind()))
        .last()
        .map(|t| t.text().to_string().trim().to_string());
    ExportedFnSig { params, ret }
}

/// Canonicalize a path for use as an index key.
///
/// Two files that are the same on-disk (e.g. accessed via a symlink) must map
/// to the SAME index key; otherwise the index accumulates duplicate / stale
/// entries for what is logically one file.
///
/// Strategy:
/// 1. Try `std::fs::canonicalize` first — it resolves symlinks and produces an
///    absolute path (the only truly correct approach for on-disk equality).
/// 2. Fall back to a **lexical** pass (strip `.`/`..` components without FS
///    access) when the path does not yet exist on disk, e.g. a newly created
///    file that the editor has notified us about but has not been flushed yet.
fn canonicalize(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    // Lexical fallback: resolve `.` / `..` without touching the filesystem.
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
    let local_decls = collect_local_decls(&tree, &resolved);

    FileIndex {
        text: text.to_string(),
        exports,
        defs,
        uses,
        imports,
        local_decls,
        parsed_ok: true,
    }
}

/// Every NON-global (frame-local) binding's `(name-token range, decl range)` — so a
/// cursor on a local DECL resolves to its frame-precise `GlobalBindingId::Local`.
/// The name-token range narrows the binding's full `decl_range` to its identifier
/// (mirroring `navigation::name_token_range_for`).
fn collect_local_decls(
    tree: &crate::syntax::cst::ResolvedNode,
    resolved: &crate::syntax::resolve::types::ResolveResult,
) -> Vec<(ByteSpan, ByteSpan)> {
    let mut out = Vec::new();
    for b in &resolved.bindings {
        if b.is_global {
            continue;
        }
        let name_tok = name_token_in_range(tree, b.decl_range, &b.name).unwrap_or(b.decl_range);
        out.push((ByteSpan::from(name_tok), ByteSpan::from(b.decl_range)));
    }
    out
}

/// The NAME-token range of `name` within the node at `decl_range` (its first
/// matching `Ident`, falling back to the first `Ident`). `None` if no node has that
/// exact range. Mirrors `navigation::name_token_range_for`.
fn name_token_in_range(
    tree: &crate::syntax::cst::ResolvedNode,
    decl_range: cstree::text::TextRange,
    name: &str,
) -> Option<cstree::text::TextRange> {
    let node = tree.descendants().find(|n| n.text_range() == decl_range)?;
    let idents: Vec<_> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .collect();
    idents
        .iter()
        .find(|t| t.text() == name)
        .or_else(|| idents.first())
        .map(|t| t.text_range())
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

/// Project the resolver's `uses` into [`UseSite`]s with FRAME-PRECISE targets
/// (DX D3 Task 11). Each `NameRef` is classified by the per-file `syntax::resolve`
/// verdict FIRST (not by a name-coarse import-membership test), so a LOCAL that
/// shadows an imported name resolves to its own decl — NOT to the import:
///
/// - `Resolution::Local`/`Upvalue` → `LocalDecl(its frame-precise DECL range)`;
/// - `Resolution::Global(name)` where `name` is an IMPORTED name → `Imported`
///   (lifts through the edge to the definer); otherwise → `GlobalDef(this file's
///   def name-range)`;
/// - everything else → `Other`.
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
    let mut import_module: HashMap<&str, Option<PathBuf>> = HashMap::new();
    for e in imports {
        for n in &e.names {
            import_module
                .entry(n.as_str())
                .or_insert_with(|| e.resolved.clone());
        }
    }
    // Map a module-global def NAME -> its name_range (for same-file GlobalDef
    // targets). Import bindings are excluded (they are not the definer).
    let mut global_def: HashMap<&str, ByteSpan> = HashMap::new();
    for d in defs {
        if d.kind != DefKind::Import {
            global_def.entry(d.name.as_str()).or_insert(d.name_range);
        }
    }

    let mut out = Vec::new();
    for nameref in tree.descendants().filter(|n| n.kind() == NameRef) {
        let Some(name) = crate::syntax::resolve::ident_text(nameref) else {
            continue;
        };
        let use_range = nameref.text_range();
        // The resolver keys its verdict by the NameRef NODE range (`use_range`), but
        // the edit/reference position must be the bare `Ident` TOKEN — a NameRef node
        // can carry leading whitespace trivia (e.g. `x` in `return x`), and using the
        // node range would make a rename eat the preceding space (`returnw`).
        let tok_range = nameref
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == Ident)
            .map(|t| t.text_range())
            .unwrap_or(use_range);
        let range = ByteSpan::from(tok_range);
        // FRAME-PRECISE: branch on the resolver's OWN verdict first.
        let target = match resolved.uses.get(&use_range) {
            // A local/upvalue use → its exact decl range (the shadowing binding).
            Some(Resolution::Local(_) | Resolution::Upvalue(_)) => {
                match frame_precise_decl(resolved, use_range) {
                    Some(decl) => ResolvedTarget::LocalDecl(ByteSpan::from(decl)),
                    None => ResolvedTarget::Other,
                }
            }
            // A module-global use. If it is an IMPORTED name, lift through the
            // edge; otherwise it is defined in THIS file.
            Some(Resolution::Global(_)) => {
                if let Some(module) = import_module.get(name.as_str()) {
                    ResolvedTarget::Imported {
                        module: module.clone(),
                        name: name.clone(),
                    }
                } else if let Some(span) = global_def.get(name.as_str()) {
                    ResolvedTarget::GlobalDef(*span)
                } else {
                    ResolvedTarget::Other
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

/// The FRAME-PRECISE declaration `TextRange` a `Local`/`Upvalue` use resolves to,
/// computed by the SAME frame walk the in-file `navigation::BindingId` uses (here
/// lifted onto the `ResolveResult` directly so the workspace index shares ONE
/// resolution model). `None` if the use is not a local/upvalue or its binding
/// cannot be located.
fn frame_precise_decl(
    resolved: &crate::syntax::resolve::types::ResolveResult,
    use_range: cstree::text::TextRange,
) -> Option<cstree::text::TextRange> {
    use crate::syntax::resolve::types::{Resolution, UpvalueDescriptor};
    let binding = match resolved.uses.get(&use_range)? {
        Resolution::Local(slot) => {
            let frame = innermost_frame_containing(resolved, use_range.start().into())?;
            binding_in_frame(resolved, frame, *slot)?
        }
        Resolution::Upvalue(idx) => {
            let mut frame = innermost_frame_containing(resolved, use_range.start().into())?;
            let mut idx = *idx as usize;
            loop {
                let info = resolved.frames.get(&frame)?;
                match info.upvalues.get(idx)? {
                    UpvalueDescriptor::ParentLocal { slot, .. } => {
                        let parent = parent_frame(resolved, frame.1)?;
                        break binding_in_frame(resolved, parent, *slot)?;
                    }
                    UpvalueDescriptor::ParentUpvalue(parent_idx) => {
                        frame = parent_frame(resolved, frame.1)?;
                        idx = *parent_idx as usize;
                    }
                }
            }
        }
        _ => return None,
    };
    Some(binding.decl_range)
}

/// The `(SyntaxKind, TextRange)` of the INNERMOST frame whose range contains `offset`.
/// Mirrors `navigation::innermost_frame_containing` over the `ResolveResult`.
fn innermost_frame_containing(
    resolved: &crate::syntax::resolve::types::ResolveResult,
    offset: usize,
) -> Option<(SyntaxKind, cstree::text::TextRange)> {
    resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: usize = r.start().into();
            let e: usize = r.end().into();
            offset >= s && offset < e
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

/// The INNERMOST frame STRICTLY containing `child` — its parent frame. Mirrors
/// `navigation::parent_frame`.
fn parent_frame(
    resolved: &crate::syntax::resolve::types::ResolveResult,
    child: cstree::text::TextRange,
) -> Option<(SyntaxKind, cstree::text::TextRange)> {
    let cs: u32 = child.start().into();
    let ce: u32 = child.end().into();
    resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: u32 = r.start().into();
            let e: u32 = r.end().into();
            s <= cs && e >= ce && (e - s) > (ce - cs)
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

/// The binding with `slot` whose OWNING frame is `frame`. Mirrors
/// `navigation::binding_in_frame`.
fn binding_in_frame(
    resolved: &crate::syntax::resolve::types::ResolveResult,
    frame: (SyntaxKind, cstree::text::TextRange),
    slot: u32,
) -> Option<&crate::syntax::resolve::types::Binding> {
    resolved.bindings.iter().find(|b| {
        !b.is_global
            && b.slot == slot
            && owning_frame_of(resolved, b.decl_range)
                .map(|f| f == frame)
                .unwrap_or(false)
    })
}

/// The innermost frame whose range CONTAINS `decl_range`. Mirrors
/// `navigation::owning_frame_of`.
fn owning_frame_of(
    resolved: &crate::syntax::resolve::types::ResolveResult,
    decl_range: cstree::text::TextRange,
) -> Option<(SyntaxKind, cstree::text::TextRange)> {
    let ds: u32 = decl_range.start().into();
    let de: u32 = decl_range.end().into();
    resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: u32 = r.start().into();
            let e: u32 = r.end().into();
            s <= ds && e >= de
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

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

    #[test]
    fn import_rewrite_on_move_points_at_new_path() {
        // a.as is imported by b.as via "./a"; moving a.as → lib/a.as rewrites b's
        // specifier to "./lib/a".
        let a = (PathBuf::from("/ws/a.as"), "export fn f() {}\n".to_string());
        let b = (
            PathBuf::from("/ws/b.as"),
            "import { f } from \"./a\"\nf()\n".to_string(),
        );
        let idx = WorkspaceIndex::build_from_files(&[a, b]);
        let edits =
            idx.import_rewrite_edits(&PathBuf::from("/ws/a.as"), &PathBuf::from("/ws/lib/a.as"));
        assert_eq!(edits.len(), 1, "{edits:?}");
        let (importer, range, new_spec) = &edits[0];
        assert_eq!(importer, &PathBuf::from("/ws/b.as"));
        assert_eq!(new_spec, "./lib/a");
        // The range is the inner specifier (between the quotes).
        let b_text = "import { f } from \"./a\"\nf()\n";
        assert_eq!(&b_text[range.start..range.end], "./a");
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
        assert!(fs
            .iter()
            .any(|d| d.kind == DefKind::Import && d.path == Path::new("/ws/b.as")));
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

    // ---- L2: cross-file go-to-definition ----------------------------------

    #[test]
    fn definition_of_b_use_of_f_points_into_a() {
        let idx = fixture();
        let b = PathBuf::from("/ws/b.as");
        let text = &idx.files[&b].text;
        // Cursor on `f` in `print(f(1))` (the use, not the import clause).
        let offset = text.rfind("f(1)").unwrap();
        let (def_path, span) = idx.definition_at(&b, offset).expect("cross-file def");
        assert_eq!(def_path, PathBuf::from("/ws/a.as"));
        // The span is `f`'s name range in a.as.
        let a_text = &idx.files[&PathBuf::from("/ws/a.as")].text;
        assert_eq!(&a_text[span.start..span.end], "f");
    }

    // ---- L3: workspace symbols + find-references ---------------------------

    #[test]
    fn workspace_symbols_match_across_files() {
        let idx = fixture();
        let syms = idx.workspace_symbols("f");
        assert!(syms.iter().any(|d| d.name == "f" && d.kind == DefKind::Fn));
        // Query filters by substring; "unrel" matches `unrelated`.
        let u = idx.workspace_symbols("unrel");
        assert!(u.iter().any(|d| d.name == "unrelated"));
        // Import re-bindings are excluded from workspace symbols.
        assert!(!idx.workspace_symbols("f").iter().any(|d| d.kind == DefKind::Import));
    }

    #[test]
    fn references_of_f_find_a_decl_and_b_use() {
        let idx = fixture();
        let a = PathBuf::from("/ws/a.as");
        let a_text = &idx.files[&a].text;
        // Cursor on `f`'s declaration in a.as.
        let offset = a_text.find("f(x)").unwrap();
        let refs = idx.references_at(&a, offset, true);
        // The decl in a + the use in b.
        assert!(
            refs.iter().any(|(p, _)| *p == a),
            "should include a's decl: {refs:?}"
        );
        assert!(
            refs.iter().any(|(p, _)| p == Path::new("/ws/b.as")),
            "should include b's use: {refs:?}"
        );
    }

    // ---- L4: rename across files ------------------------------------------

    #[test]
    fn rename_f_rewrites_decl_import_and_use() {
        let idx = fixture();
        let a = PathBuf::from("/ws/a.as");
        let b = PathBuf::from("/ws/b.as");
        let a_text = &idx.files[&a].text;
        let offset = a_text.find("f(x)").unwrap();
        let edits = idx.rename_edits(&a, offset, "g").expect("renameable");
        // The decl name in a, the import clause in b, and b's use are all edited.
        assert!(edits.iter().any(|(p, _)| *p == a), "decl edit: {edits:?}");
        let b_edits: Vec<_> = edits.iter().filter(|(p, _)| *p == b).collect();
        assert!(b_edits.len() >= 2, "import clause + use in b: {edits:?}");
    }

    #[test]
    fn rename_refused_on_collision() {
        // b imports `f` AND defines `print`-shadowing... use a real collision:
        // rename `f` to `unrelated` is fine (different file), but rename to a name
        // already a top-level def in a TOUCHED file is refused.
        let a = (
            PathBuf::from("/ws/a.as"),
            "export fn f(x) { return x }\nfn taken() { return 0 }\n".to_string(),
        );
        let b = (
            PathBuf::from("/ws/b.as"),
            "import { f } from \"./a\"\nprint(f(1))\n".to_string(),
        );
        let idx = WorkspaceIndex::build_from_files(&[a, b]);
        let a_path = PathBuf::from("/ws/a.as");
        let a_text = &idx.files[&a_path].text;
        let offset = a_text.find("f(x)").unwrap();
        // `taken` already exists in a.as → collision → refused.
        assert!(idx.rename_edits(&a_path, offset, "taken").is_none());
    }

    #[test]
    fn rename_refused_on_parse_error_in_importer() {
        let mut idx = fixture();
        let b = PathBuf::from("/ws/b.as");
        idx.reindex_file(&b, "import { f } from \"./a\"\nprint(f(@@@\n");
        let a = PathBuf::from("/ws/a.as");
        let a_text = &idx.files[&a].text;
        let offset = a_text.find("f(x)").unwrap();
        // An importer (b) has a parse error → rename refused (unsafe edit).
        assert!(idx.rename_edits(&a, offset, "g").is_none());
    }

    // ---- D-arity: index-backed file-module call-arity ---------------------

    #[test]
    fn file_module_arity_flags_wrong_arity() {
        let a = (
            PathBuf::from("/ws/a.as"),
            "export fn add(x, y) { return x }\n".to_string(),
        );
        let b_text = "import { add } from \"./a\"\nprint(add(1))\n".to_string();
        let b = (PathBuf::from("/ws/b.as"), b_text.clone());
        let idx = WorkspaceIndex::build_from_files(&[a, b]);
        let diags = idx.file_module_arity(&PathBuf::from("/ws/b.as"), &b_text);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, "call-arity");
        assert!(diags[0].message.contains("2 argument(s)"), "{:?}", diags[0]);
    }

    #[test]
    fn file_module_arity_correct_call_not_flagged() {
        let a = (
            PathBuf::from("/ws/a.as"),
            "export fn add(x, y) { return x }\n".to_string(),
        );
        let b_text = "import { add } from \"./a\"\nprint(add(1, 2))\n".to_string();
        let idx = WorkspaceIndex::build_from_files(&[
            a,
            (PathBuf::from("/ws/b.as"), b_text.clone()),
        ]);
        assert!(idx
            .file_module_arity(&PathBuf::from("/ws/b.as"), &b_text)
            .is_empty());
    }

    #[test]
    fn file_module_arity_skips_unparseable_target() {
        // The import target a.as has a parse error → arity unknown → not flagged.
        let a = (
            PathBuf::from("/ws/a.as"),
            "export fn add(x, y) { @@@ }\n".to_string(),
        );
        let b_text = "import { add } from \"./a\"\nprint(add(1))\n".to_string();
        let idx = WorkspaceIndex::build_from_files(&[
            a,
            (PathBuf::from("/ws/b.as"), b_text.clone()),
        ]);
        assert!(idx
            .file_module_arity(&PathBuf::from("/ws/b.as"), &b_text)
            .is_empty());
    }

    /// Phase 0 regression: cross-file go-to-definition must keep resolving through
    /// the LSP unification refactor (providers/model swap). A hermetic temp-dir
    /// fixture (real files on disk) builds the index and asserts a use of an
    /// imported `helper()` in `main.as` resolves to its decl in `lib.as`.
    #[test]
    fn definition_resolves_across_files_after_phase0() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.as");
        let main = dir.path().join("main.as");
        fs::write(&lib, "export fn helper() { return 1 }\n").unwrap();
        fs::write(&main, "import { helper } from \"./lib\"\nlet x = helper()\n").unwrap();
        let idx = WorkspaceIndex::build_from_files(&[
            (lib.clone(), fs::read_to_string(&lib).unwrap()),
            (main.clone(), fs::read_to_string(&main).unwrap()),
        ]);
        let text = fs::read_to_string(&main).unwrap();
        // The USE of `helper` in `helper()` (not the import clause).
        let off = text.rfind("helper").unwrap();
        let (def_path, span) = idx
            .definition_at(&canon(&main), off)
            .expect("cross-file helper() should resolve to lib.as");
        assert_eq!(def_path, canon(&lib));
        let lib_text = fs::read_to_string(&lib).unwrap();
        assert_eq!(&lib_text[span.start..span.end], "helper");
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

    // ── Task 9: workspace index for worker class / worker fn* ──────────────

    /// A `worker class` is indexed as `DefKind::Class` in the workspace index.
    #[test]
    fn worker_class_indexed_as_class() {
        let files = vec![(
            PathBuf::from("/ws/w.as"),
            "worker class Counter { fn inc(): number { return 1 } }\n".to_string(),
        )];
        let idx = WorkspaceIndex::build_from_files(&files);
        let defs = &idx.defs_by_name["Counter"];
        assert!(
            defs.iter().any(|d| d.kind == DefKind::Class),
            "worker class must be indexed as Class; got: {defs:?}"
        );
    }

    /// A `worker fn*` is indexed as `DefKind::Fn` in the workspace index.
    #[test]
    fn worker_fn_star_indexed_as_fn() {
        let files = vec![(
            PathBuf::from("/ws/w.as"),
            // Body uses `yield` directly (no for-range) so the CST parser accepts it
            // cleanly; the workspace indexer requires a parse-error-free file to index.
            "worker fn* stream(n: number) { yield n }\n".to_string(),
        )];
        let idx = WorkspaceIndex::build_from_files(&files);
        let defs = &idx.defs_by_name["stream"];
        assert!(
            defs.iter().any(|d| d.kind == DefKind::Fn),
            "worker fn* must be indexed as Fn; got: {defs:?}"
        );
    }

    /// C5 — `canonicalize` resolves symlinks when they exist on-disk (using
    /// `std::fs::canonicalize`), and falls back to lexical normalisation for
    /// paths that do not exist yet (e.g. newly-created files still being typed).
    #[test]
    fn canonicalize_resolves_symlinks_with_lexical_fallback() {
        let dir = std::env::temp_dir().join(format!("ascript-canon-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let link = dir.join("link");
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(dir.join("real"), &link);
            std::fs::write(dir.join("real/a.as"), "fn f() {}\n").unwrap();
            assert_eq!(
                canonicalize(&link.join("a.as")),
                canonicalize(&dir.join("real/a.as")),
                "symlinked and real paths must key identically"
            );
        }
        // Non-existent path: lexical fallback still normalizes `..`.
        assert_eq!(
            canonicalize(Path::new("/x/y/../z.as")),
            PathBuf::from("/x/z.as")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
