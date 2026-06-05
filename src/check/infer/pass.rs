//! The type-checking pass (SP10 §2 / §4). T1 ships a NO-OP `run` (emits nothing) so
//! the corpus zero-new-diagnostic differential is green by construction before any
//! diagnostic is load-bearing — exactly the V13-T1 "land-before-load-bearing" move.
//! T2+ fill in synthesis/checking/narrowing.

use crate::check::diagnostic::AsDiagnostic;
use crate::check::infer::table::Table;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

/// Drive the inference pass. T1: returns no diagnostics.
pub fn run(
    _tree: &ResolvedNode,
    _resolved: &ResolveResult,
    _src: &str,
    _table: &Table,
) -> Vec<AsDiagnostic> {
    Vec::new()
}
