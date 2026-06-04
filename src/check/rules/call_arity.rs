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
//! - the function has a REST parameter (`...rest`) — the MAX is unbounded, so a
//!   too-MANY call is never flagged (a too-FEW call below the min is still flagged);
//! - any call argument is a SPREAD (`f(...xs)`) — the count is unknown.
//!
//! Default parameters (SP2 §2) make arity a RANGE: `min` = the leading run of
//! params with no default; `max` = the param count (or ∞ with a rest). A call is
//! flagged when `arg_count < min` or (no rest and `arg_count > max`).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{code_range, fn_name, is_expr_kind};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution, ResolveResult};
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
        // no other (shadowing) binding of the same name. (call_arity.rs:resolves_to_fn)
        if !resolves_to_fn(callee, name.as_str(), fn_decl.text_range(), resolved) {
            continue;
        }

        // Compute the callee's arity RANGE: `min` required, `max` total (None = a
        // rest param makes the max unbounded).
        let Arity { min, max } = fn_arity(fn_decl);

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

        // Too few (below the required min) or, when the arity is bounded, too many.
        let too_few = arg_count < min;
        let too_many = max.is_some_and(|m| arg_count > m);
        if too_few || too_many {
            // Describe the expected count: an exact `N` when min == max, else a
            // range (`at least N` with a rest, `N to M` otherwise).
            let expected = match max {
                Some(m) if m == min => format!("{min} argument(s)"),
                Some(m) => format!("{min} to {m} argument(s)"),
                None => format!("at least {min} argument(s)"),
            };
            out.push(AsDiagnostic {
                range: code_range(call),
                severity: Severity::Warning,
                code: "call-arity".to_string(),
                message: format!(
                    "{name} expects {expected} but is called with {arg_count}"
                ),
                fix: None,
            });
        }
    }
    out
}

/// The arity range of a `FnDecl`: `min` leading required params (no default),
/// `max` total positional params, or `None` when a `...rest` makes it unbounded.
struct Arity {
    min: usize,
    max: Option<usize>,
}

/// The arity range of a `FnDecl`. `min` is the leading run of POSITIONAL params
/// with no default (SP2 §2: a required param may not follow a defaulted one, so
/// this is the count of params before the first defaulted one); `max` is the
/// positional param count, or `None` when a `...rest` param makes it unbounded. A
/// param is a rest iff it carries a `DotDotDot` token; it has a default iff it has
/// an EXPRESSION child (the `= expr` part — distinct from its TYPE child).
fn fn_arity(fn_decl: &ResolvedNode) -> Arity {
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Arity {
            min: 0,
            max: Some(0),
        };
    };
    let mut min = 0usize;
    let mut positional = 0usize;
    let mut seen_default = false;
    for p in list.children().filter(|c| c.kind() == Param) {
        let is_rest = p
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == DotDotDot);
        if is_rest {
            return Arity { min, max: None }; // variadic — max unbounded
        }
        positional += 1;
        let has_default = p.children().any(|c| is_expr_kind(c.kind()));
        if has_default {
            seen_default = true;
        } else if !seen_default {
            min += 1;
        }
    }
    Arity {
        min,
        max: Some(positional),
    }
}

/// Does the callee `NameRef` resolve to the genuine binding of the unique top-level
/// `fn` `name` declared at `decl_range`? True iff the resolver maps this use to an
/// in-file/global binding AND there is exactly one binding of that name, which is a
/// `Fn` binding declared at exactly `decl_range`. A `let`/`const`/parameter that
/// SHADOWS the fn name produces a second (non-`Fn`, different-range) binding, so the
/// call is correctly skipped. Mirrors `unknown_enum_variant::resolves_to_enum`.
fn resolves_to_fn(
    callee: &ResolvedNode,
    name: &str,
    decl_range: cstree::text::TextRange,
    resolved: &ResolveResult,
) -> bool {
    // The use must resolve to *some* in-file/global binding (not Unresolved/builtin).
    let bound = match resolved.uses.get(&callee.text_range()) {
        Some(Resolution::Local(_) | Resolution::Upvalue(_)) => true,
        Some(Resolution::Global(gname)) => resolved
            .bindings
            .iter()
            .any(|b| b.is_global && b.name == *gname),
        _ => false,
    };
    if !bound {
        return false;
    }
    // The name must have exactly one binding, which must be the `fn` decl — i.e. no
    // other (shadowing) binding shares the name.
    let mut same_name = resolved.bindings.iter().filter(|b| b.name == name);
    let Some(only) = same_name.next() else {
        return false;
    };
    if same_name.next().is_some() {
        return false; // ambiguous: the name is bound more than once (shadowing)
    }
    only.kind == BindingKind::Fn && only.decl_range == decl_range
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
        assert_eq!(
            count(src, "call-arity"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn too_few_args_flagged() {
        let src = "fn f(a, b) { return a }\nf(1)";
        assert_eq!(
            count(src, "call-arity"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
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
    fn param_shadow_not_flagged() {
        // `cb` inside `apply` is the PARAMETER (a 1-arg lambda passed in), not the
        // top-level `fn cb(a, b)`. Calling `cb(99)` must NOT be checked against the
        // top-level fn's arity. (BLOCKER false-positive regression.)
        let src =
            "fn cb(a, b) { return a }\nfn apply(cb) { return cb(99) }\nprint(apply((n) => n * 2))";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn block_let_shadow_not_flagged() {
        // A `let` that shadows a top-level fn name in an inner block must suppress the
        // arity check on calls that resolve to the local.
        let src = "fn g(a, b) { return a }\nfn run() {\n  let g = (x) => x\n  return g(1)\n}\nprint(run())";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn genuine_mismatch_still_flagged() {
        // The fix must not silence real mismatches to a uniquely-named top-level fn.
        let src = "fn f(a, b) { return a }\nf(1, 2, 3)";
        assert_eq!(
            count(src, "call-arity"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
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

    // ---- SP2 §2: default-parameter arity range -----------------------------

    #[test]
    fn default_param_in_range_not_flagged() {
        // `fn f(a, b = 1)` accepts 1 OR 2 args — both are in range.
        assert!(
            !has("fn f(a, b = 1) { return a }\nf(1)", "call-arity"),
            "{:?}",
            analyze("fn f(a, b = 1) { return a }\nf(1)").diagnostics
        );
        assert!(
            !has("fn f(a, b = 1) { return a }\nf(1, 2)", "call-arity"),
            "{:?}",
            analyze("fn f(a, b = 1) { return a }\nf(1, 2)").diagnostics
        );
    }

    #[test]
    fn default_param_too_few_flagged() {
        // Below the required min (1) → flagged.
        let src = "fn f(a, b = 1) { return a }\nf()";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "call-arity")
            .unwrap();
        assert_eq!(d.message, "f expects 1 to 2 argument(s) but is called with 0");
    }

    #[test]
    fn default_param_too_many_flagged() {
        // Above the max (2, no rest) → flagged.
        let src = "fn f(a, b = 1) { return a }\nf(1, 2, 3)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn default_with_rest_only_too_few_flagged() {
        // `fn f(a, b = 2, ...xs)`: min 1, max unbounded. Too-few (0) flagged; any
        // count >= 1 is in range (never too-many).
        let too_few = "fn f(a, b = 2, ...xs) { return a }\nf()";
        assert_eq!(
            count(too_few, "call-arity"),
            1,
            "{:?}",
            analyze(too_few).diagnostics
        );
        let in_range = "fn f(a, b = 2, ...xs) { return a }\nf(1)\nf(1, 2, 3, 4)";
        assert!(
            !has(in_range, "call-arity"),
            "{:?}",
            analyze(in_range).diagnostics
        );
    }
}
