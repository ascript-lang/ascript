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

    // Map each NAMED import (`import { a, b } from "<spec>"`) to its module
    // specifier, but ONLY for names imported exactly once across the file (an
    // ambiguous re-import is skipped — conservative). Namespace imports
    // (`import * as ns`) are excluded: `ns.f(...)` is a member call, not handled
    // here. Used for the std-function arity branch.
    let mut import_counts: HashMap<String, usize> = HashMap::new();
    let mut import_module: HashMap<String, String> = HashMap::new();
    for import in tree.descendants().filter(|n| n.kind() == ImportStmt) {
        let Some(list) = import.children().find(|c| c.kind() == ImportList) else {
            continue; // namespace import — skip
        };
        let Some(spec) = import_specifier(import) else {
            continue;
        };
        for t in list.children_with_tokens().filter_map(|el| el.into_token()) {
            if t.kind() == Ident {
                let n = t.text().to_string();
                *import_counts.entry(n.clone()).or_default() += 1;
                import_module.insert(n, spec.clone());
            }
        }
    }
    let import_unique = |name: &str| import_counts.get(name).copied() == Some(1);

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
        } else if import_unique(&name)
            && import_stmt_range(tree, &name).is_some_and(|decl_range| {
                // The import binding's `decl_range` is the `ImportStmt` range (the
                // resolver records every imported name at the statement). Verify
                // the use binds to the genuine unique import (no shadowing).
                resolves_to_unique(
                    callee,
                    name.as_str(),
                    decl_range,
                    BindingKind::Import,
                    resolved,
                )
            }) {
            // An imported std function: look up its curated REQUIRED arity. Only a
            // too-FEW call is flagged (native fns ignore surplus args → never
            // too-many). File-module imports are NOT handled here (path-less
            // `analyze` has no cross-file view — that is wired in the LSP/index).
            import_module
                .get(&name)
                .filter(|spec| spec.starts_with("std/"))
                .and_then(|spec| crate::check::std_arity::std_fn_arity(spec, &name))
        } else {
            None
        };
        let Some(a) = arity else {
            continue;
        };
        if let Some(d) = flag_call(call, &name, a) {
            out.push(d);
        }
    }

    // ---- Method calls `recv.m(args)` with a statically-certain receiver class.
    // A separate pass: the callee is a `MemberExpr` (NOT `OptMemberExpr`), the
    // receiver's class is provable (only `self` in a method of a unique class, OR
    // a `let`/`const` directly bound to `C(...)` and never reassigned), and `m`
    // resolves to a unique method on that class (or up the `extends` chain).
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        let Some(member) = call.children().find(|c| c.kind() == MemberExpr) else {
            continue; // not a `recv.m(...)` (a NameRef callee, or an `?.` OptMember → skip)
        };
        let Some(method_name) = member_property_name(member) else {
            continue;
        };
        // Receiver = the member's object expression (the first child before the
        // property `Ident` token).
        let Some(recv) = member.children().find(|c| is_expr_kind(c.kind())) else {
            continue;
        };
        // Determine the receiver's class NAME with certainty, else skip.
        let Some(class_name) = receiver_class(recv, tree, resolved) else {
            continue;
        };
        if !class_unique(&class_name) {
            continue;
        }
        let Some(class) = class_by_name.get(&class_name) else {
            continue;
        };
        // Resolve `m` to a unique method on the class or an ancestor; skip on any
        // ambiguity (unknown method, non-unique superclass chain).
        let Some(method) = find_method(class, &method_name, &class_by_name, &class_counts) else {
            continue;
        };
        let arity = decl_arity(&method);
        if let Some(d) = flag_call(call, &method_name, arity) {
            out.push(d);
        }
    }
    out
}

/// Emit a `call-arity` diagnostic for `call` against `arity`, or `None` if the
/// positional arg count is in range. A spread arg makes the count unknown → skip.
fn flag_call(call: &ResolvedNode, name: &str, arity: Arity) -> Option<AsDiagnostic> {
    use SyntaxKind::*;
    let Arity { min, max } = arity;
    let arg_list = call.children().find(|c| c.kind() == ArgList)?;
    if arg_list.children().any(|c| c.kind() == SpreadElem) {
        return None;
    }
    let arg_count = arg_list
        .children()
        .filter(|c| is_expr_kind(c.kind()))
        .count();
    let too_few = arg_count < min;
    let too_many = max.is_some_and(|m| arg_count > m);
    if !(too_few || too_many) {
        return None;
    }
    let expected = match max {
        Some(m) if m == min => format!("{min} argument(s)"),
        Some(m) => format!("{min} to {m} argument(s)"),
        None => format!("at least {min} argument(s)"),
    };
    Some(AsDiagnostic {
        range: code_range(call),
        severity: Severity::Warning,
        code: "call-arity".to_string(),
        message: format!("{name} expects {expected} but is called with {arg_count}"),
        fix: None,
    })
}

/// The module-specifier string of an `ImportStmt` (the `from "<spec>"` token),
/// with surrounding quotes stripped. `None` if absent/malformed.
fn import_specifier(import: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let tok = import
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == Str)?;
    let raw = tok.text();
    // Module specifiers are plain ASCII paths with no escapes — strip the quotes.
    let trimmed = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(raw);
    Some(trimmed.to_string())
}

/// The `text_range()` of the `ImportStmt` that binds the named import `name` (the
/// range the resolver uses as the import binding's `decl_range`). `None` if no
/// `import { … name … }` statement contains it.
fn import_stmt_range(tree: &ResolvedNode, name: &str) -> Option<cstree::text::TextRange> {
    use SyntaxKind::*;
    tree.descendants()
        .filter(|n| n.kind() == ImportStmt)
        .find(|import| {
            import
                .children()
                .find(|c| c.kind() == ImportList)
                .is_some_and(|list| {
                    list.children_with_tokens()
                        .filter_map(|el| el.into_token())
                        .any(|t| t.kind() == Ident && t.text() == name)
                })
        })
        .map(|n| n.text_range())
}

/// The property NAME of a `MemberExpr` `recv.name` — the LAST `Ident` token
/// directly under the member (after the `.`). `None` if absent.
fn member_property_name(member: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let idents: Vec<_> = member
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == Ident)
        .collect();
    idents.last().map(|t| t.text().to_string())
}

/// Determine the receiver expression's class NAME with STATIC CERTAINTY, else
/// `None` (skip — the zero-false-positive gate). Only two receiver shapes are
/// certain:
///
/// 1. **`self`** inside a NON-static method of a uniquely-named `class C` → `C`.
///    (A `self` in a static method has no receiver; a `self` outside a method is
///    unresolved.)
/// 2. A **`NameRef`** to a `let`/`const` whose initializer is DIRECTLY a
///    constructor call `C(...)` of a unique class AND that is NEVER reassigned
///    (`Binding.mutated == false`). Any indirection (return value, reassignment,
///    parameter, computed) → `None`.
fn receiver_class(
    recv: &ResolvedNode,
    tree: &ResolvedNode,
    resolved: &ResolveResult,
) -> Option<String> {
    use SyntaxKind::*;
    if recv.kind() != NameRef {
        return None;
    }
    let recv_name = crate::syntax::resolve::ident_text(recv)?;

    // (1) `self` → the enclosing non-static method's class.
    if recv_name == "self" {
        return self_class(recv, tree);
    }

    // (2) A `let`/`const` directly bound to `C(...)`, never reassigned. Find the
    // unique binding of this name and require it to be an immutable-enough local
    // (`!mutated`) whose declaring `LetStmt` initializer is `C(...)`.
    let mut same = resolved.bindings.iter().filter(|b| b.name == recv_name);
    let only = same.next()?;
    if same.next().is_some() {
        return None; // ambiguous (shadowed) → skip
    }
    if !matches!(only.kind, BindingKind::Let | BindingKind::Const) {
        return None;
    }
    if only.mutated {
        return None; // reassigned somewhere → class no longer certain
    }
    // Find the `LetStmt` at the binding's decl_range and read its initializer.
    let let_stmt = tree
        .descendants()
        .find(|n| n.kind() == LetStmt && n.text_range() == only.decl_range)?;
    let init = let_stmt.children().find(|c| is_expr_kind(c.kind()))?;
    // The initializer must be DIRECTLY a call `C(...)` whose callee is a NameRef.
    if init.kind() != CallExpr {
        return None;
    }
    let ctor = init.children().find(|c| c.kind() == NameRef)?;
    crate::syntax::resolve::ident_text(ctor)
}

/// The class name of the NON-static method enclosing this `self` reference, or
/// `None` if `self` is not inside an instance method (e.g. a static method, or
/// the top level). Walks ancestors to the nearest `MethodDecl`, requires it to be
/// non-static, then up to its `ClassDecl` and reads the class name.
fn self_class(recv: &ResolvedNode, _tree: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let mut node = recv.parent();
    let mut method: Option<ResolvedNode> = None;
    while let Some(n) = node {
        if n.kind() == MethodDecl {
            method = Some(n.clone());
            break;
        }
        node = n.parent();
    }
    let method = method?;
    if crate::syntax::resolve::is_static_method(&method) {
        return None; // a static method has no `self` receiver
    }
    // The class is the nearest `ClassDecl` ancestor of the method.
    let mut node = method.parent();
    while let Some(n) = node {
        if n.kind() == ClassDecl {
            return crate::syntax::resolve::ident_text(n);
        }
        node = n.parent();
    }
    None
}

/// Resolve method `name` to a unique `MethodDecl` on `class` or — walking the
/// `extends` chain — an ancestor, returning the FIRST match leaf→base (matching
/// runtime method resolution). `None` (skip) if the method is not found, or the
/// superclass chain is not fully resolvable to unique in-file classes (an
/// unknown/imported base could define or override `name` invisibly).
fn find_method(
    class: &ResolvedNode,
    name: &str,
    class_by_name: &HashMap<String, ResolvedNode>,
    class_counts: &HashMap<String, usize>,
) -> Option<ResolvedNode> {
    use SyntaxKind::*;
    let mut cur = class.clone();
    let mut depth = 0;
    loop {
        // An instance (non-static) method named `name` declared on `cur`.
        if let Some(m) = cur.children().filter(|c| c.kind() == MethodDecl).find(|m| {
            crate::syntax::resolve::ident_text(m).as_deref() == Some(name)
                && !crate::syntax::resolve::is_static_method(m)
        }) {
            return Some(m.clone());
        }
        // Not here — walk to the superclass, requiring it to be a unique in-file
        // class (else we cannot be certain the method isn't defined/overridden up
        // the chain → skip).
        let Some(sup) = superclass_name(&cur) else {
            return None; // reached a root without finding `name`
        };
        if class_counts.get(&sup).copied() != Some(1) {
            return None; // unknown/ambiguous base → can't be certain
        }
        cur = class_by_name.get(&sup)?.clone();
        depth += 1;
        if depth > 64 {
            return None; // pathological/cyclic — stay conservative
        }
    }
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

    // ---- B3: method-call arity (receiver class statically certain) --------

    #[test]
    fn method_too_many_on_let_bound_receiver_flagged() {
        // `let c = C()` directly binds a unique class; `c.m(1,2,3)` for a 1-arg
        // method panics at runtime (`m expected 1 argument(s), got 3`).
        let src = "class C { fn m(x) { return x } }\nlet c = C()\nc.m(1, 2, 3)\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_correct_arity_on_let_bound_receiver_not_flagged() {
        let src = "class C { fn m(x) { return x } }\nlet c = C()\nprint(c.m(9))\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_too_few_on_let_bound_receiver_flagged() {
        let src = "class C { fn m(a, b) { return a } }\nlet c = C()\nc.m(1)\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_on_self_inside_method_flagged() {
        // `self.m(...)` inside a method of C is checked against C's `m`.
        let src = "class C {\n fn m(x) { return x }\n fn run() { return self.m(1, 2) }\n}\nprint(C().run())\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_inherited_via_extends_flagged() {
        // `B` inherits `m` from `A`; a wrong-arity call on a `B`-typed receiver
        // is checked against the inherited method.
        let src = "class A { fn m(x) { return x } }\nclass B extends A {}\nlet b = B()\nb.m(1, 2, 3)\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_rest_param_only_too_few_flagged() {
        let src = "class C { fn m(a, ...rest) { return a } }\nlet c = C()\nc.m(1, 2, 3, 4)\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
        let few = "class C { fn m(a, ...rest) { return a } }\nlet c = C()\nc.m()\n";
        assert_eq!(count(few, "call-arity"), 1, "{:?}", analyze(few).diagnostics);
    }

    // ---- B3 must-NOT-flag cases (zero false positives) --------------------

    #[test]
    fn method_reassigned_receiver_not_flagged() {
        // `c` is reassigned after the C() init → its class is no longer certain.
        let src =
            "class C { fn m(x) { return x } }\nclass D { fn m(a, b, c) { return a } }\nlet c = C()\nc = D()\nc.m(1, 2, 3)\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_receiver_from_return_value_not_flagged() {
        // `let c = make()` — the receiver class is unknown (return value).
        let src =
            "class C { fn m(x) { return x } }\nfn make() { return C() }\nlet c = make()\nc.m(1, 2, 3)\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_optional_call_not_flagged() {
        // `c?.m(...)` is an OptMemberExpr callee → skip.
        let src = "class C { fn m(x) { return x } }\nlet c = C()\nc?.m(1, 2, 3)\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_param_receiver_not_flagged() {
        // A parameter receiver has unknown class → skip.
        let src = "class C { fn m(x) { return x } }\nfn use(c) { return c.m(1, 2, 3) }\nprint(use(C()))\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_unknown_name_not_flagged() {
        // `m` is not a method of C → can't prove a mismatch → skip.
        let src = "class C { fn m(x) { return x } }\nlet c = C()\nc.other(1, 2, 3)\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn method_self_in_static_not_flagged() {
        // `self` in a STATIC method has no receiver → no class → skip.
        let src = "class C {\n fn m(x) { return x }\n static fn s() { return self.m(1, 2) }\n}\nprint(C.s())\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    // ---- B4: imported std-function arity (too-few only) -------------------

    #[test]
    fn imported_std_fn_too_few_flagged() {
        // `abs` requires 1 numeric arg; calling it with 0 is a guaranteed runtime
        // panic (`math.abs expects a number, got nil`).
        let src = "import { abs } from \"std/math\"\nprint(abs())\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn imported_std_fn_correct_arity_not_flagged() {
        let src = "import { abs } from \"std/math\"\nprint(abs(-1))\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn imported_std_fn_extra_args_not_flagged() {
        // Native fns IGNORE surplus args — `abs(-1, 99)` does NOT panic, so it
        // must NOT be flagged (zero-false-positive).
        let src = "import { abs } from \"std/math\"\nprint(abs(-1, 99))\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn imported_std_pow_too_few_flagged() {
        let src = "import { pow } from \"std/math\"\nprint(pow(2))\n";
        assert_eq!(count(src, "call-arity"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn imported_variadic_std_fn_not_flagged() {
        // `max` is variadic/overloaded → absent from the table → never flagged.
        let src = "import { max } from \"std/math\"\nprint(max())\n";
        assert!(!has(src, "call-arity"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn shadowed_std_import_name_not_flagged() {
        // A local `let abs` shadowing the import suppresses the std-arity check.
        let src =
            "import { abs } from \"std/math\"\nfn f() {\n let abs = (x) => x\n return abs()\n}\nprint(f())\n";
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
