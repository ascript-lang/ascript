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
