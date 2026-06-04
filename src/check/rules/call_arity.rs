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
//!
//! Record construction (SP2 §5): a `CallExpr` whose callee resolves to a unique
//! in-file `ClassDecl` is a constructor call. If the class (or any ancestor in a
//! fully-resolvable in-file chain) declares an instance `init`, the call is
//! checked against THAT `init`'s param arity (its params, never `self`). If NO
//! class in the chain declares `init`, the class auto-derives a positional
//! constructor over its MERGED declared fields (base-class-first): `min` =
//! required fields (no default), `max` = total fields. Conservative: the whole
//! call is skipped unless the entire superclass chain is uniquely-resolvable
//! in-file classes (an unknown/imported base means unknown fields).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{
    code_range, decl_arity, fn_name, is_expr_kind, resolves_to_unique, Arity,
};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, ResolveResult};
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

    // Same idea for classes: name → ClassDecl, only for names declared exactly once
    // (a class call `C(args)` is a constructor — SP2 §5 records / `init` arity).
    let mut class_counts: HashMap<String, usize> = HashMap::new();
    let mut class_by_name: HashMap<String, ResolvedNode> = HashMap::new();
    for c in tree.descendants().filter(|n| n.kind() == ClassDecl) {
        if let Some(name) = crate::syntax::resolve::ident_text(c) {
            *class_counts.entry(name.clone()).or_default() += 1;
            class_by_name.insert(name, c.clone());
        }
    }
    let class_unique = |name: &str| class_counts.get(name).copied() == Some(1);

    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        // The callee must be a plain `NameRef` directly under the call (a method
        // call `x.m(...)` has a `MemberExpr` callee → skip).
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let name = crate::syntax::resolve::ident_text(callee).unwrap_or_default();

        // Resolve the callee to EITHER a unique top-level `fn` (function-arity) OR a
        // unique in-file `class` (constructor-arity, SP2 §5). A name can't be both
        // (single binding required for either), so try the fn path first, then the
        // class path; skip the call if neither applies.
        let arity = if unique(&name)
            && by_name.get(&name).is_some_and(|fn_decl| {
                // The use must bind to the GENUINE top-level `fn` (not a shadowing
                // `let`/`const`/param) — the shared uniqueness gate.
                resolves_to_unique(
                    callee,
                    name.as_str(),
                    fn_decl.text_range(),
                    BindingKind::Fn,
                    resolved,
                )
            }) {
            Some(decl_arity(&by_name[&name]))
        } else if class_unique(&name)
            && class_by_name.get(&name).is_some_and(|cls| {
                resolves_to_unique(
                    callee,
                    name.as_str(),
                    cls.text_range(),
                    BindingKind::Class,
                    resolved,
                )
            }) {
            // Constructor arity: an inherited/explicit `init`'s params, or — if no
            // class in the chain defines `init` — the merged declared-field count.
            // `None` means the chain isn't fully in-file resolvable → skip.
            class_arity(&class_by_name[&name], &class_by_name, &class_counts)
        } else {
            None
        };
        let Some(Arity { min, max }) = arity else {
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

/// The superclass name of a `ClassDecl` (the `Ident` after the soft keyword
/// `extends`), or `None` for a class without `extends`. Mirrors
/// `resolve::record_superclass_use`'s token walk.
fn superclass_name(class: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    class
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .skip_while(|t| !(t.kind() == Ident && t.text() == "extends"))
        .filter(|t| t.kind() == Ident)
        .nth(1)
        .map(|t| t.text().to_string())
}

/// The instance `init` `MethodDecl` of a class, if it declares one (not a static
/// method — auto-init / construction only consider instance `init`).
fn instance_init(class: &ResolvedNode) -> Option<ResolvedNode> {
    use SyntaxKind::*;
    class
        .children()
        .filter(|c| c.kind() == MethodDecl)
        .find(|m| {
            crate::syntax::resolve::ident_text(m).as_deref() == Some("init")
                && !crate::syntax::resolve::is_static_method(m)
        })
        .cloned()
}

/// The constructor arity of a class call (SP2 §5). Returns `None` (skip the call)
/// when the class's superclass chain is NOT fully resolvable to unique in-file
/// classes — an unknown/imported base means unknown inherited fields/`init`.
///
/// - If ANY class in the chain declares an instance `init`, the call is checked
///   against the LEAF-resolved `init`'s params (`init` is inherited, so the first
///   one found walking leaf→base wins; its arity is its param list, never `self`).
/// - Otherwise (no `init` anywhere) the class auto-derives a positional
///   constructor over the MERGED fields, base-class-FIRST: `min` = fields without
///   a default, `max` = total field count.
fn class_arity(
    class: &ResolvedNode,
    class_by_name: &HashMap<String, ResolvedNode>,
    class_counts: &HashMap<String, usize>,
) -> Option<Arity> {
    // Walk leaf → base, collecting the chain. Bail (None) on any unresolvable or
    // non-unique superclass, or a cycle (defensive depth cap).
    let mut chain: Vec<ResolvedNode> = Vec::new();
    let mut cur = class.clone();
    loop {
        chain.push(cur.clone());
        let Some(sup) = superclass_name(&cur) else {
            break; // reached a root class — chain fully in-file
        };
        if class_counts.get(&sup).copied() != Some(1) {
            return None; // base is unknown/imported/ambiguous → can't count fields
        }
        let parent = class_by_name.get(&sup)?;
        if chain.len() > 64 {
            return None; // pathological/cyclic — stay conservative
        }
        cur = parent.clone();
    }
    // `init` inherited: leaf→base, first one wins (matches `find_method`).
    if let Some(init) = chain.iter().find_map(instance_init) {
        return Some(decl_arity(&init));
    }
    // No `init`: auto-init over MERGED fields, base-class FIRST (reverse leaf→base).
    // `merged_field_schema` dedups by name with `IndexMap::insert`: a re-declared
    // name keeps its FIRST-seen (base) POSITION but takes the LAST-written (leaf)
    // schema — so a subclass override decides the field's default-ness. We mirror
    // that: iterate base→leaf, recording each name's latest `has_default`; the
    // distinct-name count is `max`, the count of still-required names is `min`.
    use SyntaxKind::*;
    let mut field_default: indexmap::IndexMap<String, bool> = indexmap::IndexMap::new();
    for c in chain.iter().rev() {
        for field in c.children().filter(|n| n.kind() == FieldDecl) {
            let Some(fname) = crate::syntax::resolve::ident_text(field) else {
                continue;
            };
            // A field has a default iff it carries an EXPRESSION child (the `= expr`,
            // distinct from its TYPE child) — same test as `fn_arity`'s `has_default`.
            let has_default = field.children().any(|c| is_expr_kind(c.kind()));
            field_default.insert(fname, has_default);
        }
    }
    let total = field_default.len();
    let required = field_default.values().filter(|&&d| !d).count();
    Some(Arity {
        min: required,
        max: Some(total),
    })
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

    // ---- SP2 §5: record / auto-init constructor arity ----------------------

    #[test]
    fn record_construction_too_few_flagged() {
        // A field-only class auto-derives a constructor over its fields: `Point(1)`
        // is too few (2 required).
        let src = "class Point { x: number\n y: number }\nPoint(1)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "call-arity")
            .unwrap();
        assert_eq!(d.message, "Point expects 2 argument(s) but is called with 1");
    }

    #[test]
    fn record_construction_in_range_not_flagged() {
        let src = "class Point { x: number\n y: number }\nPoint(1, 2)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn record_construction_too_many_flagged() {
        let src = "class Point { x: number\n y: number }\nPoint(1, 2, 3)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn record_defaulted_field_is_a_range() {
        // A defaulted field makes the count a 1..2 range.
        let one = "class P { x: number\n y: number = 0 }\nP(1)";
        let two = "class P { x: number\n y: number = 0 }\nP(1, 2)";
        assert!(!has(one, "call-arity"), "{:?}", analyze(one).diagnostics);
        assert!(!has(two, "call-arity"), "{:?}", analyze(two).diagnostics);
        let none = "class P { x: number\n y: number = 0 }\nP()";
        assert_eq!(
            count(none, "call-arity"),
            1,
            "{:?}",
            analyze(none).diagnostics
        );
        let three = "class P { x: number\n y: number = 0 }\nP(1, 2, 3)";
        assert_eq!(
            count(three, "call-arity"),
            1,
            "{:?}",
            analyze(three).diagnostics
        );
    }

    #[test]
    fn class_with_init_validates_against_init_params_not_fields() {
        // A class WITH an explicit init is checked against the INIT's params, NOT
        // the field count. Here init takes 1 arg though there are 2 fields, so
        // `C(5)` is fine and `C(5, 6)` is too many.
        let ok = "class C { x: number\n y: number = 0\n fn init(v) { self.x = v } }\nC(5)";
        assert!(!has(ok, "call-arity"), "{:?}", analyze(ok).diagnostics);
        let too_many = "class C { x: number\n fn init(v) { self.x = v } }\nC(5, 6)";
        assert_eq!(
            count(too_many, "call-arity"),
            1,
            "{:?}",
            analyze(too_many).diagnostics
        );
    }

    #[test]
    fn record_inheritance_merged_field_arity() {
        // Base fields then subclass fields, no init anywhere → merged count (a=1,
        // b=1) = 2 required. `B(1)` too few; `B(1, 2)` ok; `B(1, 2, 3)` too many.
        let base = "class A { a: number }\nclass B extends A { b: number }\n";
        let too_few = format!("{base}B(1)");
        let ok = format!("{base}B(1, 2)");
        let too_many = format!("{base}B(1, 2, 3)");
        assert_eq!(count(&too_few, "call-arity"), 1, "{:?}", analyze(&too_few).diagnostics);
        assert!(!has(&ok, "call-arity"), "{:?}", analyze(&ok).diagnostics);
        assert_eq!(count(&too_many, "call-arity"), 1, "{:?}", analyze(&too_many).diagnostics);
    }

    #[test]
    fn record_unknown_superclass_skipped() {
        // An imported/unknown base means unknown inherited fields → conservatively
        // skip (no false positive). `Base` is not declared in-file.
        let src = "class B extends Base { b: number }\nB(1, 2, 3, 4)";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn zero_field_class_extra_arg_flagged() {
        let src = "class E {}\nE(1)";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "call-arity")
            .unwrap();
        assert_eq!(d.message, "E expects 0 argument(s) but is called with 1");
    }
}
