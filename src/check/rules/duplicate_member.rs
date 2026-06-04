//! `duplicate-member` (structural): flag two members with the SAME name in one
//! class body — a silent shadow at runtime (the later member wins), almost always
//! a copy-paste bug. Purely structural: no resolver needed.
//!
//! A "member" is a `FieldDecl` (`name: T`) or a `MethodDecl` (`fn name(...)`).
//! Within each `ClassDecl` body the member names are collected in source order; the
//! SECOND (and any further) occurrence of a name is flagged at the duplicate's span.
//! Field-vs-method collisions count too (`x: number` then `fn x() {}` share the
//! name `x`).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;
use std::collections::HashSet;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    let mut out = Vec::new();
    for class in tree.descendants().filter(|n| n.kind() == ClassDecl) {
        let class_name = crate::syntax::resolve::ident_text(class).unwrap_or_default();
        // Static methods are a SEPARATE namespace from instance members (SP1 §3):
        // an instance `fn x` and a `static fn x` may coexist (`c.x()` vs `C.x()`).
        // A duplicate only fires WITHIN one namespace — fields + instance methods in
        // the instance set, static methods in the static set.
        let mut seen_instance: HashSet<String> = HashSet::new();
        let mut seen_static: HashSet<String> = HashSet::new();
        for member in class
            .children()
            .filter(|c| matches!(c.kind(), FieldDecl | MethodDecl))
        {
            // The member's declared name is its first `Ident` token. (For a
            // `MethodDecl`, `static`/`async`/`fn`/`*` are keyword/punct tokens, not
            // `Ident`, so the method name is the first `Ident`.)
            let Some(name) = crate::syntax::resolve::ident_text(member) else {
                continue;
            };
            let is_static = member.kind() == MethodDecl
                && crate::syntax::resolve::is_static_method(member);
            // `static fn from` is reserved (collides with the built-in typed-parse
            // `.from`) — a clear static-analysis diagnostic mirroring the
            // compile/resolve error both engines raise (SP1 §3).
            if is_static && name == "from" {
                out.push(AsDiagnostic {
                    range: code_range(member),
                    severity: Severity::Warning,
                    code: "reserved-static-member".to_string(),
                    message: "'from' is reserved on classes (collides with the built-in typed-parse `.from`)"
                        .to_string(),
                    fix: None,
                });
                continue;
            }
            let seen = if is_static {
                &mut seen_static
            } else {
                &mut seen_instance
            };
            if !seen.insert(name.clone()) {
                out.push(AsDiagnostic {
                    range: code_range(member),
                    severity: Severity::Warning,
                    code: "duplicate-member".to_string(),
                    message: format!("duplicate member '{name}' in class {class_name}"),
                    fix: None,
                });
            }
        }
    }
    out
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
    fn duplicate_field_flagged() {
        let src = "class C {\n  x: number\n  x: string\n}";
        assert_eq!(
            count(src, "duplicate-member"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn duplicate_method_flagged() {
        let src = "class C {\n  fn m() {}\n  fn m() {}\n}";
        assert_eq!(
            count(src, "duplicate-member"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn field_method_collision_flagged() {
        let src = "class C {\n  x: number\n  fn x() {}\n}";
        assert_eq!(
            count(src, "duplicate-member"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn two_static_methods_same_name_flagged() {
        // SP1 §3: a duplicate fires WITHIN the static namespace.
        let src = "class C {\n  static fn x() {}\n  static fn x() {}\n}";
        assert_eq!(
            count(src, "duplicate-member"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn instance_and_static_same_name_not_flagged() {
        // SP1 §3: static vs instance are SEPARATE namespaces — `c.x()` vs `C.x()`.
        let src = "class C {\n  fn x() {}\n  static fn x() {}\n}";
        assert!(
            !has(src, "duplicate-member"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn distinct_members_not_flagged() {
        let src = "class C {\n  x: number\n  y: string\n  fn m(){}\n}";
        assert!(
            !has(src, "duplicate-member"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn static_fn_from_flagged_reserved() {
        // SP1 §3: `static fn from` collides with the built-in typed-parse `.from`.
        let src = "class C {\n  static fn from() { return 1 }\n}";
        assert_eq!(
            count(src, "reserved-static-member"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
        // It is NOT counted as a duplicate-member.
        assert_eq!(count(src, "duplicate-member"), 0);
    }

    #[test]
    fn message_names_member_and_class() {
        let src = "class C {\n  x: number\n  x: string\n}";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "duplicate-member")
            .unwrap();
        assert_eq!(d.message, "duplicate member 'x' in class C");
    }
}
