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
        let mut seen: HashSet<String> = HashSet::new();
        for member in class
            .children()
            .filter(|c| matches!(c.kind(), FieldDecl | MethodDecl))
        {
            // The member's declared name is its first `Ident` token. (For a
            // `MethodDecl`, `async`/`fn`/`*` are keyword/punct tokens, not `Ident`,
            // so the method name is the first `Ident`.)
            let Some(name) = crate::syntax::resolve::ident_text(member) else {
                continue;
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
    fn distinct_members_not_flagged() {
        let src = "class C {\n  x: number\n  y: string\n  fn m(){}\n}";
        assert!(
            !has(src, "duplicate-member"),
            "{:?}",
            analyze(src).diagnostics
        );
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
