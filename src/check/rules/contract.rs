//! `contract-mismatch` (conservative): flag a literal argument that is PROVABLY
//! the wrong primitive for an annotated parameter — e.g. `f("x")` for
//! `fn f(n: number)`, or `nil` for a non-`T?` param. Silent on anything uncertain.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{
    code_range, fn_name, is_type_kind, lit_name, literal_kind, resolves_to_unique, type_compat,
    Compat,
};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, ResolveResult};
use std::collections::HashMap;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Map fn name → its FnDecl node, but ONLY for names declared exactly once
    // (ambiguous/overloaded-by-shadowing names are skipped — conservative).
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
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();
        if !unique(&name) {
            continue;
        }
        let Some(fn_decl) = by_name.get(&name) else {
            continue;
        };
        // The callee must resolve to the GENUINE top-level `fn` we matched by name —
        // not a `let`/`const`/parameter that legally SHADOWS that name in an inner
        // scope. Mirrors `unknown_enum_variant::resolves_to_enum`: verify the resolved
        // use binds to a `Fn` binding declared at exactly this `FnDecl`'s range, with
        // no other (shadowing) binding of the same name. (contract.rs:resolves_to_fn)
        if !resolves_to_unique(
            callee,
            name.as_str(),
            fn_decl.text_range(),
            BindingKind::Fn,
            resolved,
        ) {
            continue;
        }

        let params = param_types(fn_decl);
        // If the fn has a rest param, only fixed positions are safe to check.
        let fixed = params.len();
        let Some(arg_list) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        // A spread arg makes positions uncertain → skip the whole call.
        if arg_list.children().any(|c| c.kind() == SpreadElem) {
            continue;
        }
        let args: Vec<_> = arg_list.children().filter(|c| is_expr(c.kind())).collect();

        for (i, arg) in args.iter().enumerate() {
            if i >= fixed {
                break; // beyond fixed params (rest) — unknown types
            }
            let Some(lit) = literal_kind(arg) else {
                continue;
            }; // only literals
            let Some(ptype) = &params[i] else {
                continue;
            }; // only annotated params
            if type_compat(ptype, lit) == Compat::No {
                out.push(AsDiagnostic {
                    range: code_range(arg),
                    severity: Severity::Warning,
                    code: "contract-mismatch".to_string(),
                    message: format!(
                        "argument {} of `{name}` is a {} literal but the parameter is declared `{}`",
                        i + 1,
                        lit_name(lit),
                        ptype.text().to_string().trim()
                    ),
                    fix: None,
                });
            }
        }
    }
    out
}

/// Per-parameter declared type node (None if a param is unannotated or is a rest).
fn param_types(fn_decl: &ResolvedNode) -> Vec<Option<ResolvedNode>> {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == Param)
        // A rest param (`...x`) ends the fixed positions.
        .take_while(|p| {
            !p.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot)
        })
        .map(|p| p.children().find(|c| is_type_kind(c.kind())).cloned())
        .collect()
}

fn is_expr(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | TemplateExpr
            | OptMemberExpr
            | TryExpr
            | UnwrapExpr
            | TernaryExpr
            | AwaitExpr
            | YieldExpr
            | MatchExpr
            | RangeExpr
    )
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    /// Does the RULE emit `code` directly? Since TYPE, the end-to-end `analyze`
    /// pipeline DROPS a `contract-mismatch` advisory at a span where the inference
    /// pass emits a blocking `type-mismatch` Error (subsumption). These unit tests
    /// exercise the rule's own detection, so they invoke it directly.
    fn has(src: &str, code: &str) -> bool {
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        super::check(&tree, &resolved, src).iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_wrong_primitive_literal() {
        let src = "fn f(n: number) { return n }\nf(\"x\")\n";
        assert!(
            has(src, "contract-mismatch"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn flags_nil_to_non_optional() {
        let src = "fn f(n: number) { return n }\nf(nil)\n";
        assert!(has(src, "contract-mismatch"));
    }

    #[test]
    fn correct_literal_not_flagged() {
        let src = "fn f(n: number) { return n }\nf(42)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn optional_accepts_nil() {
        let src = "fn f(n: number?) { return n }\nf(nil)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn union_member_accepts() {
        let src = "fn f(x: number | string) { return x }\nf(\"ok\")\nf(1)\n";
        assert!(!has(src, "contract-mismatch"));
    }

    #[test]
    fn any_and_unannotated_and_nonliteral_silent() {
        // `any` accepts; unannotated param: silent; non-literal arg: silent.
        let src = "fn a(x: any) { return x }\nfn b(y) { return y }\nlet v = 1\nfn c(n: number) { return n }\na(\"s\")\nb(\"s\")\nc(v)\n";
        assert!(
            !has(src, "contract-mismatch"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn param_shadow_not_flagged() {
        // `cb` inside `apply` is the PARAMETER (a 1-arg lambda taking a string), not
        // the top-level `fn cb(n: number)`. `cb("hello")` must NOT be checked against
        // the top-level fn's contract. (BLOCKER false-positive regression.)
        let src = "fn cb(n: number) { return n }\nfn apply(cb) { return cb(\"hello\") }\nprint(apply((s) => s))\n";
        assert!(
            !has(src, "contract-mismatch"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn block_let_shadow_not_flagged() {
        let src = "fn h(n: number) { return n }\nfn run() {\n  let h = (s) => s\n  return h(\"x\")\n}\nprint(run())\n";
        assert!(
            !has(src, "contract-mismatch"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn genuine_mismatch_still_flagged() {
        let src = "fn f(n: number) { return n }\nf(\"x\")\n";
        assert!(
            has(src, "contract-mismatch"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn class_typed_param_is_silent() {
        // a named class type — a literal can't be proven wrong → silent.
        let src = "class User {}\nfn f(u: User) { return u }\nf(1)\n";
        assert!(!has(src, "contract-mismatch"));
    }
}
