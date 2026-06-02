//! Lint rules. Each is `fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`.
use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

pub mod missing_return;
pub mod shadowing;
pub mod undefined;
pub mod unreachable;
pub mod unused;

pub type Rule = fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>;

/// All enabled rules. (Each C2 task fills in its rule body.)
pub static ALL: &[Rule] = &[
    undefined::check,
    unused::check,
    shadowing::check,
    unreachable::check,
    missing_return::check,
];
