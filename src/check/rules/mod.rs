//! Lint rules. Each is `fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>`.
use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

pub mod dead_recover;
pub mod ignored_result;
pub mod missing_return;
pub mod shadowing;
pub mod unawaited;
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
    unawaited::check,
    ignored_result::check,
    dead_recover::check,
];

/// The `CallExpr` directly dropped by an `ExprStmt` (result unused). `None` if the
/// statement's expression isn't a bare call (e.g. it's `await f()`, `x = f()`,
/// `f()?`, `f()!`, or `return f()` — those wrap the call in another node).
pub fn dropped_call(expr_stmt: &ResolvedNode) -> Option<ResolvedNode> {
    use crate::syntax::kind::SyntaxKind;
    if expr_stmt.kind() != SyntaxKind::ExprStmt {
        return None;
    }
    expr_stmt
        .children()
        .find(|c| c.kind() == SyntaxKind::CallExpr)
        .cloned()
}
