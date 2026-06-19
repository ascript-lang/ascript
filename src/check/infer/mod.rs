//! SP10 — the advisory static gradual type checker.
//!
//! A single stateful inference pass integrated into the analysis driver with the
//! same signature as a [`crate::check::rules::Rule`]. It is feature-independent,
//! static-only, reuses the CST front-end, and NEVER instantiates the interpreter.
//!
//! Sub-modules: [`ty`] (the `CheckTy` lattice + three-valued `assignable`/`join`),
//! [`table`] (the class/enum symbol table), [`env`] (the inferred-binding
//! environment + narrowing overlay), and [`pass`] (the synthesis/checking/
//! narrowing visitor that emits diagnostics).

pub mod elide;
pub mod env;
pub mod pass;
pub mod table;
pub mod ty;
pub mod unify;

use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

/// Run the type-checker pass over a resolved CST and return its diagnostics.
///
/// Same signature as a `Rule`, so the driver invokes it exactly where the
/// `rules::ALL` loop runs. Builds the class/enum table once, then drives the
/// inference pass.
pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, src: &str) -> Vec<AsDiagnostic> {
    let table = table::Table::build(tree, resolved);
    pass::run(tree, resolved, src, &table)
}

/// ELIDE (§4.1): run the CST front-end + the inference pass in proof-COLLECTION mode
/// over `src` and return its [`elide::ElisionSet`] — every call/let/fn-return site
/// that satisfies the strict (E)∧(Y)∧(A) predicate (spec §2). Diagnostic-neutral
/// (§6.5): the diagnostics this walk would emit are byte-identical to [`check`]'s and
/// are DISCARDED here. Runs NO code (never instantiates the interpreter); the
/// per-module set scoping is by construction (one module's source in, one set out).
pub fn elision_proofs(src: &str) -> elide::ElisionSet {
    use crate::syntax::{resolve, tree_builder};
    let tree = tree_builder::build_tree(crate::syntax::parser::parse(src));
    let resolved = resolve::resolve(&tree);
    let table = table::Table::build(&tree, &resolved);
    pass::collect_elision(&tree, &resolved, src, &table)
}

/// ELIDE diagnostic-neutrality gate (§6.5): the `(diagnostics, set)` of running the
/// pass in COLLECTION mode over `src`. The diagnostics MUST equal [`check`]'s
/// byte-for-byte (the collector is a pure side-accumulator) — asserted by
/// `tests/elide.rs` over the whole example corpus. NOT a production path.
pub fn check_with_elision(src: &str) -> (Vec<AsDiagnostic>, elide::ElisionSet) {
    use crate::syntax::{resolve, tree_builder};
    let tree = tree_builder::build_tree(crate::syntax::parser::parse(src));
    let resolved = resolve::resolve(&tree);
    let table = table::Table::build(&tree, &resolved);
    pass::collect_elision_with_diagnostics(&tree, &resolved, src, &table)
}

/// ELIDE (§4.1, given a pre-built tree/resolve): the [`elide::ElisionSet`] for a
/// module already parsed and resolved (the loader/compiler path, which has the tree
/// in hand). Identical proof to [`elision_proofs`], no re-parse.
pub fn elision_proofs_for(
    tree: &ResolvedNode,
    resolved: &ResolveResult,
    src: &str,
) -> elide::ElisionSet {
    let table = table::Table::build(tree, resolved);
    pass::collect_elision(tree, resolved, src, &table)
}

/// Pre-built artifacts from the inference pass needed by the LSP hover provider.
///
/// Holds only owned, `Send + Sync` data (a `Vec` of `ByteSpan + String` pairs).
/// `Table` is intentionally excluded: `CheckTy::EnumVariant` carries an
/// `Rc<str>`, making `Table` `!Send`; and the hover use-case needs only the
/// RENDERED strings that `collect_hover_types` already produced.
pub struct InferArtifacts {
    /// All name-use hover spans for the file (byte range → rendered `CheckTy`).
    pub hovers: Vec<pass::HoverType>,
}

// Compile-time gate: `InferArtifacts` must be `Send + Sync` so that
// `OnceLock<InferArtifacts>` can live on `SemanticModel` (which is `Send + Sync`).
const _: fn() = || {
    fn a<T: Send + Sync>() {}
    a::<InferArtifacts>();
};

/// Build the [`InferArtifacts`] for a file from its already-parsed/resolved tree.
/// No re-parse: the callers (typically the LSP `SemanticModel`) supply the tree and
/// resolve result they already hold. The `Table` is built internally and dropped
/// after hover collection (it is `!Send` and is not stored).
pub fn build_artifacts(
    tree: &crate::syntax::cst::ResolvedNode,
    resolved: &crate::syntax::resolve::types::ResolveResult,
    src: &str,
) -> InferArtifacts {
    let table = table::Table::build(tree, resolved);
    let hovers = pass::collect_hover_types(tree, resolved, src, &table);
    InferArtifacts { hovers }
}

/// Like [`hover_type_at`] but operates on pre-built [`InferArtifacts`] (the cached
/// path). No re-parse, no re-resolve, no re-infer — just a linear scan over the
/// already-collected hover spans.
///
/// When several spans contain the offset the NARROWEST (innermost) wins, matching
/// `hover_type_at`'s contract exactly.
pub fn hover_type_in(artifacts: &InferArtifacts, byte_offset: usize) -> Option<String> {
    artifacts
        .hovers
        .iter()
        .filter(|h| byte_offset >= h.range.start && byte_offset < h.range.end)
        .min_by_key(|h| h.range.end - h.range.start)
        .map(|h| h.ty.clone())
}

/// The inferred/declared type (rendered `CheckTy`) of the name use whose byte span
/// contains `byte_offset`, if any. Runs the CST front-end + the SP10 inference pass
/// in hover-collection mode (NO interpreter). Used by the LSP hover hook.
///
/// When several recorded spans contain the offset, the NARROWEST (innermost) wins,
/// so a precise reference is preferred over an enclosing one.
///
/// **Prefer [`hover_type_in`] with a cached [`InferArtifacts`] when the caller
/// already has a [`crate::lsp::model::SemanticModel`]** — this function re-parses and
/// re-resolves on every call.
pub fn hover_type_at(src: &str, byte_offset: usize) -> Option<String> {
    use crate::syntax::{resolve, tree_builder};
    let tree = tree_builder::build_tree(crate::syntax::parser::parse(src));
    let resolved = resolve::resolve(&tree);
    let artifacts = build_artifacts(&tree, &resolved, src);
    hover_type_in(&artifacts, byte_offset)
}

#[cfg(test)]
mod infer_cache_tests {
    use super::*;

    #[test]
    fn hover_type_in_matches_hover_type_at() {
        let src = "let n: int = 1\nfn f(a: string) { return a }\nlet y = f(\"hi\")\n";
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        let artifacts = build_artifacts(&tree, &resolved, src);
        for off in 0..src.len() {
            assert_eq!(
                hover_type_in(&artifacts, off),
                hover_type_at(src, off),
                "mismatch at byte offset {off}"
            );
        }
    }

    /// Guards the NARROWEST-span selection of `hover_type_in` directly, with a
    /// synthetic set of overlapping spans. The parity test above cannot catch a bug
    /// in the SHARED selection logic (both sides route through `hover_type_in`); this
    /// one would fail if `min_by_key` ever became `max_by_key` (innermost vs outermost).
    #[test]
    fn hover_type_in_prefers_the_narrowest_overlapping_span() {
        use crate::check::diagnostic::ByteSpan;
        use crate::check::infer::pass::HoverType;
        // Three spans all containing offset 12: widths 30, 10, 4 — narrowest is "inner".
        let artifacts = InferArtifacts {
            hovers: vec![
                HoverType { range: ByteSpan { start: 0, end: 30 }, ty: "outer".into() },
                HoverType { range: ByteSpan { start: 8, end: 18 }, ty: "middle".into() },
                HoverType { range: ByteSpan { start: 10, end: 14 }, ty: "inner".into() },
            ],
        };
        assert_eq!(hover_type_in(&artifacts, 12).as_deref(), Some("inner"));
        // At offset 9 only outer+middle contain it → middle (narrower) wins.
        assert_eq!(hover_type_in(&artifacts, 9).as_deref(), Some("middle"));
        // Outside every span → None.
        assert_eq!(hover_type_in(&artifacts, 40), None);
    }
}
