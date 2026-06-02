//! `dead-recover` (Hint): a `recover(fn)` whose body provably cannot panic, so the
//! recover is inert. (Body filled in by Task 3.)

use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(_tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    Vec::new()
}
