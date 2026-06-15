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

/// The inferred/declared type (rendered `CheckTy`) of the name use whose byte span
/// contains `byte_offset`, if any. Runs the CST front-end + the SP10 inference pass
/// in hover-collection mode (NO interpreter). Used by the LSP hover hook.
///
/// When several recorded spans contain the offset, the NARROWEST (innermost) wins,
/// so a precise reference is preferred over an enclosing one.
pub fn hover_type_at(src: &str, byte_offset: usize) -> Option<String> {
    use crate::syntax::{resolve, tree_builder};
    let tree = tree_builder::build_tree(crate::syntax::parser::parse(src));
    let resolved = resolve::resolve(&tree);
    let table = table::Table::build(&tree, &resolved);
    let hovers = pass::collect_hover_types(&tree, &resolved, src, &table);
    hovers
        .into_iter()
        .filter(|h| byte_offset >= h.range.start && byte_offset < h.range.end)
        .min_by_key(|h| h.range.end - h.range.start)
        .map(|h| h.ty)
}
