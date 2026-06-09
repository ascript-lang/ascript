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
