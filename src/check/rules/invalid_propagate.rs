//! `invalid-propagate` (conservative): a postfix `?` propagate operator
//! (`TryExpr`) used inside a function whose **declared** return type is not a
//! `Result` pair. Closes the long-unenforced promise in the design spec
//! (`2026-05-29-ascript-design.md:257`: "Using `?` in a function that does not
//! return a Result pair is a compile-time error").
//!
//! Conservative — only flags when the enclosing function carries a return-type
//! ANNOTATION whose type is provably NOT a `Result`:
//!
//! - `?` with no enclosing function at all (top-level / module body) → silent
//!   (the top-level `?` semantics are a separate concern, not provably wrong).
//! - `?` in a function with NO return-type annotation → silent (cannot be
//!   statically proven to violate the contract).
//! - `?` in a function annotated `: Result<…>` (anywhere in the return type,
//!   including a union like `Result<T> | nil`) → silent (valid).
//! - `?` in a function annotated with a non-`Result` return type (`: number`,
//!   `: string`, …) → flagged.
//!
//! The nearest enclosing function is the closest `FnDecl`/`MethodDecl` ancestor;
//! an arrow expression (`ArrowExpr`) also counts as a function boundary, but
//! arrows have no return-type annotation in the surface syntax, so a `?` inside
//! an arrow is treated like an unannotated function → silent.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for try_expr in tree.descendants().filter(|n| n.kind() == TryExpr) {
        // The nearest function-like ancestor. Stop at the first arrow/fn/method:
        // an arrow boundary means the `?` is NOT governed by an outer annotated
        // fn, and arrows are unannotated, so we conservatively skip.
        let Some(func) = try_expr
            .ancestors()
            .find(|a| matches!(a.kind(), FnDecl | MethodDecl | ArrowExpr))
        else {
            continue; // top-level `?` — not provably wrong here
        };
        if func.kind() == ArrowExpr {
            continue; // arrows carry no return annotation → unprovable
        }
        // Only enforce when the function HAS a return-type annotation.
        let Some(ret_type) = func.children().find(|c| c.kind() == RetType) else {
            continue; // unannotated → conservative skip
        };
        if mentions_result(ret_type) {
            continue; // `: Result<…>` (or a union containing it) → valid
        }
        out.push(AsDiagnostic {
            range: code_range(try_expr),
            severity: Severity::Warning,
            code: "invalid-propagate".to_string(),
            message: "`?` requires the enclosing function to return a Result".to_string(),
            fix: None,
        });
    }
    out
}

/// Does this return-type subtree denote (or contain) a `Result<…>`? True when
/// any `GenericType`/`NamedType` within `ret_type` has `Result` as its head
/// identifier — covering both a bare `: Result<T>` and a union member such as
/// `: Result<T> | nil`. Keyed on the first non-trivia `Ident` token of each type
/// node, mirroring how `cst_type` (`src/compile/mod.rs`) resolves a type head.
fn mentions_result(ret_type: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    ret_type
        .descendants()
        .filter(|n| matches!(n.kind(), GenericType | NamedType))
        .any(|n| {
            n.children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == Ident)
                .is_some_and(|t| t.text() == "Result")
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
    fn non_result_return_flagged() {
        let src = "fn g(): Result<number> { return [1, nil] }\nfn f(): number { return g()? }\n";
        assert_eq!(
            count(src, "invalid-propagate"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn result_return_not_flagged() {
        let src =
            "fn g(): Result<number> { return [1, nil] }\nfn f(): Result<number> { return g()? }\n";
        assert!(
            !has(src, "invalid-propagate"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn unannotated_fn_not_flagged() {
        let src = "fn g(): Result<number> { return [1, nil] }\nfn f() { return g()? }\n";
        assert!(
            !has(src, "invalid-propagate"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn top_level_try_not_flagged() {
        let src = "fn g(): Result<number> { return [1, nil] }\nlet x = g()?\n";
        assert!(
            !has(src, "invalid-propagate"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn result_in_union_return_not_flagged() {
        let src = "fn g(): Result<number> { return [1, nil] }\nfn f(): Result<number> | nil { return g()? }\n";
        assert!(
            !has(src, "invalid-propagate"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn try_in_arrow_not_flagged() {
        // An arrow inside an annotated fn: the `?` is governed by the arrow
        // (unannotated) boundary, so it is conservatively skipped.
        let src = "fn g(): Result<number> { return [1, nil] }\nfn f(): number { let h = () => g()?\n return 1 }\n";
        assert!(
            !has(src, "invalid-propagate"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn method_non_result_return_flagged() {
        let src = "fn g(): Result<number> { return [1, nil] }\nclass C { fn m(): number { return g()? } }\n";
        assert_eq!(
            count(src, "invalid-propagate"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_is_stable() {
        let src = "fn g(): Result<number> { return [1, nil] }\nfn f(): number { return g()? }\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "invalid-propagate")
            .unwrap();
        assert_eq!(
            d.message,
            "`?` requires the enclosing function to return a Result"
        );
    }
}
