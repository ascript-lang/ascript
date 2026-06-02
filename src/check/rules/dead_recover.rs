//! `dead-recover` (Hint): a `recover(fn)` whose body provably cannot panic, so the
//! recover is inert. VERY conservative: only flags when the arrow body contains no
//! calls (a call might panic), no `!` (force-unwrap can panic), and no field
//! assignments (a typed-field assignment can panic). Such a body can only produce
//! values that never panic, so the recover does nothing.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        // callee must be the bare name `recover`
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        if crate::syntax::resolve::ident_text(callee).as_deref() != Some("recover") {
            continue;
        }
        // first arg must be an arrow whose body cannot panic
        let Some(args) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        let Some(arrow) = args.children().find(|c| c.kind() == ArrowExpr) else {
            continue;
        };
        if body_cannot_panic(arrow) {
            out.push(AsDiagnostic {
                range: ByteSpan::from(call.text_range()),
                severity: Severity::Hint,
                code: "dead-recover".to_string(),
                message: "this `recover` wraps a body that cannot panic; it has no effect"
                    .to_string(),
                fix: None,
            });
        }
    }
    out
}

/// Conservative: no calls, no `!`, no member/index assignments anywhere in the body.
fn body_cannot_panic(arrow: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    !arrow.descendants().any(|n| {
        matches!(n.kind(), CallExpr | UnwrapExpr)
            || (n.kind() == AssignExpr
                && n.children()
                    .next()
                    .map(|t| matches!(t.kind(), MemberExpr | IndexExpr))
                    .unwrap_or(false))
    })
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_inert_recover() {
        // body just returns a literal — cannot panic
        let src = "let r = recover(() => 1)\n";
        assert!(has(src, "dead-recover"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn recover_with_a_call_not_flagged() {
        let src = "fn risky() { return 1 }\nlet r = recover(() => risky())\n";
        assert!(!has(src, "dead-recover"));
    }

    #[test]
    fn recover_with_unwrap_not_flagged() {
        let src = "let r = recover(() => some_pair()!)\n";
        assert!(!has(src, "dead-recover"));
    }
}
