//! `call-arity` (conservative): flag a call with the wrong number of arguments to
//! a DIRECTLY-NAMED, UNIQUELY-RESOLVED function — mirroring the guaranteed runtime
//! Tier-2 panic `<name> expected <N> argument(s), got <M>` (spec §3.1) at
//! author-time.
//!
//! Detection: a `CallExpr` whose callee is a plain `NameRef` that the resolver
//! binds to exactly ONE in-scope/top-level **function declaration** (`FnDecl`)
//! with a FIXED parameter list. Flag when the positional arg count differs from
//! the declared param count.
//!
//! Conservative — the node is skipped on any ambiguity:
//! - callee is not a plain name (a method call `x.m(...)`, a computed callee);
//! - the name is unresolved / a bare global builtin / an import / a parameter /
//!   has multiple decls / is shadowed (only a unique file-declared `fn` proceeds);
//! - the function has a REST parameter (`...rest`) — arity is a range, not exact;
//! - any call argument is a SPREAD (`f(...xs)`) — the count is unknown.
//!
//! (AScript has no default-parameter syntax — `fn f(a, b = 1)` does not parse —
//! so there is no default-value case to skip.)

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{code_range, fn_name, is_expr_kind};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};
use std::collections::HashMap;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Map fn name → its FnDecl node, but ONLY for names declared exactly once in
    // the file. An ambiguous/overloaded-by-shadowing name is skipped entirely
    // (conservative) — same approach as `contract.rs`.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut by_name: HashMap<String, ResolvedNode> = HashMap::new();
    for f in tree.descendants().filter(|n| n.kind() == FnDecl) {
        if let Some(name) = fn_name(f) {
            *counts.entry(name.clone()).or_default() += 1;
            by_name.insert(name, f.clone());
        }
    }
    let unique = |name: &str| counts.get(name).copied() == Some(1);

    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        // The callee must be a plain `NameRef` directly under the call (a method
        // call `x.m(...)` has a `MemberExpr` callee → skip).
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();

        // The callee must resolve to a binding DECLARED in this file — a
        // local/upvalue OR a module-scope user-global (a top-level `fn`, the
        // common case) — AND be a uniquely-named declared function (a bare
        // builtin/import/parameter is excluded). Mirrors `contract.rs`.
        let user_fn = match resolved.uses.get(&callee.text_range()) {
            Some(Resolution::Local(_) | Resolution::Upvalue(_)) => true,
            Some(Resolution::Global(gname)) => resolved
                .bindings
                .iter()
                .any(|b| b.is_global && &b.name == gname),
            _ => false,
        };
        if !user_fn || !unique(&name) {
            continue;
        }
        let Some(fn_decl) = by_name.get(&name) else {
            continue;
        };

        // A rest parameter makes the arity a range, not exact → skip.
        let Some(param_count) = fixed_param_count(fn_decl) else {
            continue;
        };

        let Some(arg_list) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        // A spread arg makes the count unknown → skip the whole call.
        if arg_list.children().any(|c| c.kind() == SpreadElem) {
            continue;
        }
        let arg_count = arg_list
            .children()
            .filter(|c| is_expr_kind(c.kind()))
            .count();

        if arg_count != param_count {
            out.push(AsDiagnostic {
                range: code_range(call),
                severity: Severity::Warning,
                code: "call-arity".to_string(),
                message: format!(
                    "{name} expects {param_count} argument(s) but is called with {arg_count}"
                ),
                fix: None,
            });
        }
    }
    out
}

/// The number of FIXED (positional) parameters of a `FnDecl`, or `None` if it has
/// a REST parameter (`...name`) — in which case the arity is a range and the call
/// must not be flagged. A param is a rest iff it carries a `DotDotDot` token
/// (same detection as `contract.rs::param_types`).
fn fixed_param_count(fn_decl: &ResolvedNode) -> Option<usize> {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Some(0); // no param list ⇒ zero params
    };
    let mut count = 0usize;
    for p in list.children().filter(|c| c.kind() == Param) {
        let is_rest = p
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == DotDotDot);
        if is_rest {
            return None; // variadic — arity not exact
        }
        count += 1;
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    fn count(src: &str, code: &str) -> usize {
        analyze(src)
            .diagnostics
            .iter()
            .filter(|d| d.code == code)
            .count()
    }
    fn has(src: &str, code: &str) -> bool {
        count(src, code) > 0
    }

    #[test]
    fn too_many_args_flagged() {
        let src = "fn f(a, b) { return a }\nf(1, 2, 3)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn too_few_args_flagged() {
        let src = "fn f(a, b) { return a }\nf(1)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn correct_arity_not_flagged() {
        let src = "fn f(a, b) { return a }\nf(1, 2)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn rest_param_not_flagged() {
        // `...rest` parses (verified); arity is a range → never flagged.
        let src = "fn f(a, ...rest) { return a }\nf(1,2,3)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn unresolved_callee_not_flagged() {
        let src = "f(1,2,3)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_call_not_flagged() {
        let src = "obj.m(1,2,3)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn spread_arg_not_flagged() {
        // `f(...xs)` parses as a SpreadElem in the arg list → count unknown → skip.
        let src = "fn f(a,b){a}\nf(...xs)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn message_names_the_fn_and_counts() {
        let src = "fn f(a, b) { return a }\nf(1, 2, 3)";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "call-arity")
            .unwrap();
        assert_eq!(d.message, "f expects 2 argument(s) but is called with 3");
    }
}
