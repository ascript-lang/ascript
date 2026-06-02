//! `ignored-result`: a dropped call to a function whose declared return type is
//! `Result<…>` — the `[value, err]` pair is discarded, so the error is silently
//! ignored. (Body filled in by Task 2.)

use crate::check::diagnostic::AsDiagnostic;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(_tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    Vec::new()
}
