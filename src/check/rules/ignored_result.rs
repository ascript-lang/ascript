//! `ignored-result`: a dropped call to a function whose declared return type is
//! `Result<…>` — the `[value, err]` pair is discarded, so the error is silently
//! ignored. Conservative: only functions with an explicit `Result<…>` return
//! type (statically known); a `?`/`!`/assignment/return consumes the result and
//! is not flagged (those wrap the call, so `dropped_call` returns None).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{code_range, dropped_call};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashSet;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    // names of functions whose declared return type is `Result<…>`
    let result_fns: HashSet<String> = tree
        .descendants()
        .filter(|n| n.kind() == FnDecl && returns_result(n))
        .filter_map(fn_name)
        .collect();
    if result_fns.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for es in tree.descendants().filter(|n| n.kind() == ExprStmt) {
        let Some(call) = dropped_call(es) else {
            continue;
        };
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
        let is_local = matches!(
            resolved.uses.get(&callee.text_range()),
            Some(Resolution::Local(_) | Resolution::Upvalue(_))
        );
        if is_local && result_fns.contains(&name) {
            out.push(AsDiagnostic {
                range: code_range(&call),
                severity: Severity::Warning,
                code: "ignored-result".to_string(),
                message: format!(
                    "the Result of `{name}` is ignored; handle it with `?`, `!`, or by inspecting `[value, err]`"
                ),
                fix: None,
            });
        }
    }
    out
}

/// A `FnDecl` whose `RetType` is a `Result<…>` generic type.
fn returns_result(fn_decl: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let Some(rt) = fn_decl.children().find(|c| c.kind() == RetType) else {
        return false;
    };
    rt.children().any(|t| {
        t.kind() == GenericType
            && t.children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|tk| tk.kind() == Ident)
                .map(|tk| tk.text() == "Result")
                .unwrap_or(false)
    })
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
    fn flags_dropped_result() {
        let src = "fn load(): Result<number> { return Ok(1) }\nfn main() { load() }\nmain()\n";
        assert!(has(src, "ignored-result"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn propagated_or_unwrapped_not_flagged() {
        let src = "fn load(): Result<number> { return Ok(1) }\nfn a(): Result<number> { load()?\n return Ok(2) }\nfn b() { load()! }\n";
        assert!(!has(src, "ignored-result"));
    }

    #[test]
    fn non_result_fn_not_flagged() {
        let src = "fn plain(): number { return 1 }\nfn main() { plain() }\nmain()\n";
        assert!(!has(src, "ignored-result"));
    }
}
