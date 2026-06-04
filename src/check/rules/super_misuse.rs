//! `super-misuse` (structural): flag `super` used inside a class that has NO
//! superclass — a guaranteed runtime panic at the `super.<m>(...)` call site
//! (`no superclass method '<m>' (no superclass)`).
//!
//! `super` is lexed as a plain `Ident` token used as the receiver of a member
//! access (`super.init()`), so it appears in the CST as a `NameRef` node whose
//! ident text is `super`. We walk those `NameRef`s, find the nearest enclosing
//! `ClassDecl`, and flag if that class has no `extends` clause. A class records
//! its superclass as `[ClassName, "extends", SuperName]` (where `extends` is a
//! soft keyword parsed as an `Ident` token) — so "has a superclass" iff one of
//! the class's direct `Ident` tokens is the text `extends`.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    let mut out = Vec::new();
    for name_ref in tree.descendants().filter(|n| n.kind() == NameRef) {
        if crate::syntax::resolve::ident_text(name_ref).as_deref() != Some("super") {
            continue;
        }
        // Find the nearest enclosing class declaration. Skip if there is none
        // (a bare `super` outside a class is a separate compile error, not ours).
        let Some(class) = name_ref.ancestors().find(|a| a.kind() == ClassDecl) else {
            continue;
        };
        if class_has_superclass(class) {
            continue;
        }
        let class_name = crate::syntax::resolve::ident_text(class).unwrap_or_default();
        out.push(AsDiagnostic {
            range: code_range(name_ref),
            severity: Severity::Warning,
            code: "super-misuse".to_string(),
            message: format!("`super` used in class {class_name}, which has no superclass"),
            fix: None,
        });
    }
    out
}

/// A class has a superclass iff its direct token stream contains the soft keyword
/// `extends` (parsed as an `Ident` token). Only the class header tokens are direct
/// children of `ClassDecl` before the body; member tokens live under `FieldDecl`/
/// `MethodDecl` child NODES, so a `children_with_tokens` token scan only sees the
/// header — `extends` cannot be confused with anything in the body.
fn class_has_superclass(class: &ResolvedNode) -> bool {
    class
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::Ident && t.text() == "extends")
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

    #[test]
    fn super_without_superclass_flagged() {
        let src = "class A {\n  fn init() { super.init() }\n}";
        assert_eq!(count(src, "super-misuse"), 1, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn super_with_superclass_ok() {
        let src = "class A {}\nclass B extends A {\n  fn init() { super.init() }\n}";
        assert_eq!(count(src, "super-misuse"), 0, "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn message_names_the_class() {
        let src = "class A {\n  fn init() { super.init() }\n}";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "super-misuse")
            .unwrap();
        assert_eq!(d.message, "`super` used in class A, which has no superclass");
    }

    #[test]
    fn no_super_no_flag() {
        let src = "class A {\n  fn init() { self.x = 1 }\n}";
        assert_eq!(count(src, "super-misuse"), 0, "{:?}", analyze(src).diagnostics);
    }
}
