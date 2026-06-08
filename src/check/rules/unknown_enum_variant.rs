//! `unknown-enum-variant` (conservative): flag access of a member that does NOT
//! exist on a statically-known enum — mirroring the guaranteed runtime panic
//! `enum <E> has no variant '<V>'` (spec §3.2) at author-time.
//!
//! Detection: a `MemberExpr` `<recv>.<member>` whose `<recv>` is a plain `NameRef`
//! that the resolver binds to exactly ONE in-file `enum` declaration (a unique,
//! NON-shadowed/NON-reassigned `EnumDecl`), and whose `<member>` is not one of that
//! enum's variant names. Flag it.
//!
//! Conservative — the node is skipped on any ambiguity:
//! - the receiver is not a plain name (a chained/computed receiver);
//! - the name does not resolve to a binding whose decl is the unique enum decl
//!   (an unrelated value, a shadowed/reassigned name, a different `EnumDecl` —
//!   anything other than the one enum proceeds is skipped);
//! - the member name is already a declared variant (correct access).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution, ResolveResult};
use std::collections::{HashMap, HashSet};

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Map enum name → (decl text_range, variant-name set), but ONLY for names
    // declared exactly once as an enum in the file. An enum name that is declared
    // more than once (any shadowing) is dropped entirely — conservative.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut by_name: HashMap<String, (cstree::text::TextRange, HashSet<String>)> = HashMap::new();
    for e in tree.descendants().filter(|n| n.kind() == EnumDecl) {
        let Some(name) = decl_ident(e) else {
            continue;
        };
        *counts.entry(name.clone()).or_default() += 1;
        by_name.insert(name, (e.text_range(), variant_names(e)));
    }
    // Drop any enum name that is declared more than once.
    by_name.retain(|name, _| counts.get(name).copied() == Some(1));

    let mut out = Vec::new();
    for member in tree.descendants().filter(|n| n.kind() == MemberExpr) {
        // The receiver must be a plain `NameRef` directly under the member access
        // (a chained `a.b.c` receiver is a `MemberExpr` child → skip).
        let Some(recv) = member.children().find(|c| c.kind() == NameRef) else {
            continue;
        };
        let recv_name = crate::syntax::resolve::ident_text(recv).unwrap_or_default();

        // The receiver must resolve, via the resolver, to a binding whose name is
        // our unique enum AND whose declaration site is exactly that enum's decl
        // (so a value that merely shares the enum's name — a shadowing `let` — is
        // NOT mistaken for the enum). The resolver records `kind == Enum` only for
        // the genuine enum binding.
        let Some((decl_range, variants)) = by_name.get(&recv_name) else {
            continue;
        };
        if !resolves_to_enum(recv, recv_name.as_str(), *decl_range, resolved) {
            continue;
        }

        // The member name is the `MemberExpr`'s own `Ident` token (`<recv>.<member>`
        // — the receiver's name lives inside the child `NameRef`, so this reads the
        // member, not the receiver).
        let Some(member_name) = crate::syntax::resolve::ident_text(member) else {
            continue;
        };
        if variants.contains(&member_name) {
            continue; // a real variant — correct access
        }

        out.push(AsDiagnostic {
            range: code_range(member),
            severity: Severity::Warning,
            code: "unknown-enum-variant".to_string(),
            message: format!("enum {recv_name} has no variant '{member_name}'"),
            fix: None,
        });
    }

    // ADT §7.2(b): qualified variant PATTERNS `Shape.Nonexist(r)` in match position.
    // A bare variant pattern (`Nonexist(r)`) has no enum receiver to resolve against
    // and is NOT covered here (it surfaces via exhaustiveness / the binding-shadow
    // diagnostic). A payload-constructor CALL `Shape.Nope(…)` already flows through the
    // `MemberExpr` arm above (its callee is a `MemberExpr`), so it needs no new handling.
    for vp in tree.descendants().filter(|n| n.kind() == VariantPat) {
        let Some((enum_ref, variant_name, variant_node)) = qualified_variant_pat(vp) else {
            continue; // bare (unqualified) variant pattern — skip (conservative)
        };
        let Some((decl_range, variants)) = by_name.get(&enum_ref) else {
            continue; // receiver is not our unique enum
        };
        // The enum ref must resolve, via the resolver, to the genuine enum binding.
        if !resolves_to_enum(&variant_node, enum_ref.as_str(), *decl_range, resolved) {
            // `resolves_to_enum` consults `resolved.uses` by the node's text_range; for a
            // VariantPat the enum-ref token is not a `NameRef` use, so fall back to the
            // unique-binding check (no shadow/reassign), mirroring its tail.
            if !enum_ref_is_unique_binding(enum_ref.as_str(), *decl_range, resolved) {
                continue;
            }
        }
        if variants.contains(&variant_name) {
            continue; // a real variant — correct
        }
        out.push(AsDiagnostic {
            range: code_range(vp),
            severity: Severity::Warning,
            code: "unknown-enum-variant".to_string(),
            message: format!("enum {enum_ref} has no variant '{variant_name}'"),
            fix: None,
        });
    }
    out
}

/// For a QUALIFIED `VariantPat` (`Shape.Nonexist(…)`), return
/// `(enum_name, variant_name, variant_pat_node)`. The enum/variant refs are the two
/// leading `Ident` tokens (before the first `(`), separated by `.`. A BARE variant
/// pattern (`Circle(…)`, a single leading Ident) returns `None`.
fn qualified_variant_pat(vp: &ResolvedNode) -> Option<(String, String, ResolvedNode)> {
    use SyntaxKind::*;
    let mut idents = Vec::new();
    for el in vp.children_with_tokens() {
        if let Some(tok) = el.into_token() {
            if tok.kind() == LParen {
                break;
            }
            if tok.kind() == Ident {
                idents.push(tok.text().to_string());
            }
        }
    }
    if idents.len() >= 2 {
        Some((idents[0].clone(), idents[1].clone(), vp.clone()))
    } else {
        None
    }
}

/// Is `name` bound exactly ONCE, as the enum declared at `decl_range`? (The
/// non-resolver tail of [`resolves_to_enum`], for nodes whose enum-ref token is not a
/// recorded `NameRef` use — e.g. a `VariantPat`'s leading enum ident.)
fn enum_ref_is_unique_binding(
    name: &str,
    decl_range: cstree::text::TextRange,
    resolved: &ResolveResult,
) -> bool {
    let mut same_name = resolved.bindings.iter().filter(|b| b.name == name);
    let Some(only) = same_name.next() else {
        return false;
    };
    if same_name.next().is_some() {
        return false;
    }
    only.kind == BindingKind::Enum && only.decl_range == decl_range
}

/// The declared name of a decl node — its first `Ident` token.
fn decl_ident(node: &ResolvedNode) -> Option<String> {
    crate::syntax::resolve::ident_text(node)
}

/// The set of variant names declared in an `EnumDecl` (each `EnumVariant`'s first
/// `Ident` token).
fn variant_names(enum_decl: &ResolvedNode) -> HashSet<String> {
    use SyntaxKind::*;
    enum_decl
        .children()
        .filter(|c| c.kind() == EnumVariant)
        .filter_map(crate::syntax::resolve::ident_text)
        .collect()
}

/// Does the receiver `NameRef` resolve to the genuine binding of the unique enum
/// `name` declared at `decl_range`? True iff the resolver maps this use to a
/// Local/Upvalue/Global binding AND there is an `Enum`-kind binding of that name
/// declared at exactly `decl_range` (so a reassigned/shadowed name that happens to
/// match the enum's name is rejected).
fn resolves_to_enum(
    recv: &ResolvedNode,
    name: &str,
    decl_range: cstree::text::TextRange,
    resolved: &ResolveResult,
) -> bool {
    // The use must resolve to *some* in-file/global binding (not Unresolved).
    let resolution = resolved.uses.get(&recv.text_range());
    let bound = matches!(
        resolution,
        Some(Resolution::Local(_) | Resolution::Upvalue(_) | Resolution::Global(_))
    );
    if !bound {
        return false;
    }
    // And the name must have exactly one binding, which must be the enum decl —
    // i.e. no other (shadowing/reassigning) binding shares the name.
    let mut same_name = resolved.bindings.iter().filter(|b| b.name == name);
    let Some(only) = same_name.next() else {
        return false;
    };
    if same_name.next().is_some() {
        return false; // ambiguous: the name is bound more than once
    }
    only.kind == BindingKind::Enum && only.decl_range == decl_range
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
    fn unknown_variant_flagged() {
        let src = "enum Color { Red, Green }\nprint(Color.Reddd)";
        assert_eq!(
            count(src, "unknown-enum-variant"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn known_variant_not_flagged() {
        let src = "enum Color { Red, Green }\nprint(Color.Red)";
        assert!(
            !has(src, "unknown-enum-variant"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn unknown_receiver_not_flagged() {
        let src = "enum Color { Red, Green }\nprint(other.Reddd)";
        assert!(
            !has(src, "unknown-enum-variant"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn shadowed_name_not_flagged() {
        let src = "let Color = 5\nprint(Color.x)";
        assert!(
            !has(src, "unknown-enum-variant"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_names_enum_and_variant() {
        let src = "enum Color { Red, Green }\nprint(Color.Reddd)";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "unknown-enum-variant")
            .unwrap();
        assert_eq!(d.message, "enum Color has no variant 'Reddd'");
    }

    // ADT §7.2 extensions.

    #[test]
    fn payload_ctor_call_unknown_variant_flagged() {
        // `Shape.Nope(1)` — the callee `Shape.Nope` is a MemberExpr, already covered.
        let src = "enum Shape { Circle, Point }\nlet x = Shape.Nope(1)\nprint(x)";
        assert_eq!(count(src, "unknown-enum-variant"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn qualified_variant_pattern_unknown_flagged() {
        let src = "enum Shape { Circle, Point }\nfn f(s: Shape): number {\n  return match s {\n    Shape.Nonexist(r) => 1,\n    _ => 2,\n  }\n}\nprint(f(Shape.Circle))";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "unknown-enum-variant");
        assert!(d.is_some(), "{:?}", analyze(src).diagnostics);
        assert_eq!(d.unwrap().message, "enum Shape has no variant 'Nonexist'");
    }

    #[test]
    fn qualified_variant_pattern_known_not_flagged() {
        let src = "enum Shape { Circle(radius: float), Point }\nfn f(s: Shape): float {\n  return match s {\n    Shape.Circle(r) => r,\n    Shape.Point => 0.0,\n  }\n}\nprint(f(Shape.Point))";
        assert!(!has(src, "unknown-enum-variant"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn bare_variant_pattern_not_flagged_by_this_rule() {
        // A BARE `Nonexist(r)` has no enum receiver — NOT covered by this rule.
        let src = "enum Shape { Circle(radius: float), Point }\nfn f(s: Shape): float {\n  return match s {\n    Circle(r) => r,\n    _ => 0.0,\n  }\n}\nprint(f(Shape.Point))";
        assert!(!has(src, "unknown-enum-variant"), "{:?}", analyze(src).diagnostics);
    }
}
