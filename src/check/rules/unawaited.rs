//! `unawaited-future`: a dropped result of a call to a locally-declared `async fn`.
//! A script `async fn` call returns a `future<T>` that is eagerly scheduled but
//! cancelled-on-drop — dropping it (a bare call statement, not `await`ed) is the
//! M17 leak class and almost always a bug. Conservative: only locally-declared
//! async fns called by bare name (not methods, not stdlib detach like `task.spawn`).

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::check::rules::dropped_call;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashSet;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    // names of locally-declared async functions
    let async_fns: HashSet<String> = tree
        .descendants()
        .filter(|n| n.kind() == FnDecl && is_async(n))
        .filter_map(fn_name)
        .collect();
    if async_fns.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for es in tree.descendants().filter(|n| n.kind() == ExprStmt) {
        let Some(call) = dropped_call(es) else {
            continue;
        };
        // callee must be a bare NameRef that resolves to a local/upvalue binding
        // (so a same-named global isn't mistaken) whose name is an async fn.
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
        let is_local = matches!(
            resolved.uses.get(&callee.text_range()),
            Some(Resolution::Local(_) | Resolution::Upvalue(_))
        );
        if is_local && async_fns.contains(&name) {
            out.push(AsDiagnostic {
                range: ByteSpan::from(call.text_range()),
                severity: Severity::Warning,
                code: "unawaited-future".to_string(),
                message: format!(
                    "the future returned by `{name}` is dropped; did you mean `await {name}(...)`?"
                ),
                fix: None,
            });
        }
    }
    out
}

fn is_async(fn_decl: &ResolvedNode) -> bool {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AsyncKw)
}

fn fn_name(fn_decl: &ResolvedNode) -> Option<String> {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_dropped_async_call() {
        let src = "async fn work() { return 1 }\nfn main() { work() }\nmain()\n";
        assert!(has(src, "unawaited-future"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn awaited_call_not_flagged() {
        let src = "async fn work() { return 1 }\nasync fn main() { await work() }\n";
        assert!(!has(src, "unawaited-future"));
    }

    #[test]
    fn assigned_or_returned_not_flagged() {
        let src = "async fn work() { return 1 }\nfn a() { let f = work() }\nfn b() { return work() }\n";
        assert!(!has(src, "unawaited-future"));
    }

    #[test]
    fn non_async_call_not_flagged() {
        let src = "fn work() { return 1 }\nfn main() { work() }\nmain()\n";
        assert!(!has(src, "unawaited-future"));
    }
}
