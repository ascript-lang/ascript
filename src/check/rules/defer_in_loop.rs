//! `defer-in-loop` (Warning, default-on, §6.1): fires when a `DeferStmt` is
//! lexically inside a `while`/`for` body **within the same function** — the
//! defers are registered once per iteration and all drain at the single function
//! exit, which is almost always unintentional. A nested `fn`/arrow body RESETS
//! the walk: its defers are per-call of the closure, not the enclosing loop.
//!
//! Walk strategy: for every `DeferStmt` in the tree, walk its ANCESTORS; fire if
//! we reach a loop node (`WhileStmt`/`ForStmt`) before we reach a function
//! boundary (`FnDecl`/`MethodDecl`/`ArrowExpr`) or the tree root.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for defer in tree.descendants().filter(|n| n.kind() == DeferStmt) {
        if inside_loop_same_fn(defer) {
            out.push(AsDiagnostic {
                range: code_range(defer),
                severity: Severity::Warning,
                code: "defer-in-loop".to_string(),
                message: "'defer' inside a loop registers one call per iteration; they all run at function exit — wrap the loop body in a function if you want per-iteration cleanup".to_string(),
                fix: None,
            });
        }
    }
    out
}

/// True if `defer_node` is lexically inside a `WhileStmt` or `ForStmt` body
/// WITHOUT an intervening function boundary (`FnDecl`, `MethodDecl`, `ArrowExpr`).
fn inside_loop_same_fn(defer_node: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let mut cur = defer_node.parent();
    while let Some(node) = cur {
        match node.kind() {
            // Reached a loop body without crossing a function boundary → fire.
            WhileStmt | ForStmt => return true,
            // Crossed a function boundary before any loop → do NOT fire.
            FnDecl | MethodDecl | ArrowExpr => return false,
            _ => {}
        }
        cur = node.parent();
    }
    // Reached the tree root without finding a loop → not inside a loop.
    false
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
    fn fires_inside_while_loop() {
        let src = "fn f() { while (true) { defer print(1) } }\n";
        assert!(has(src, "defer-in-loop"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_range() {
        let src = "fn f() { for (i in 1..10) { defer print(i) } }\n";
        assert!(has(src, "defer-in-loop"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_of() {
        let src = "fn f(xs) { for (x of xs) { defer print(x) } }\n";
        assert!(has(src, "defer-in-loop"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn fires_inside_for_await() {
        // for await uses the same ForStmt node in the CST
        let src = "async fn f(g) { for await (x of g) { defer print(x) } }\n";
        assert!(has(src, "defer-in-loop"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn no_fire_outside_any_loop() {
        let src = "fn f() { defer print(1) }\n";
        assert!(!has(src, "defer-in-loop"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn no_fire_for_defer_in_nested_fn_inside_loop() {
        // The nested fn body resets the walk — its defer is per-call, not per-loop-iter.
        let src = "fn outer() { while (true) { fn inner() { defer print(1) } inner() } }\n";
        assert!(
            !has(src, "defer-in-loop"),
            "nested fn inside loop should NOT fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_for_defer_in_nested_arrow_inside_loop() {
        // Arrow body also resets the walk.
        let src = "fn outer() { while (true) { let f = () => { defer print(1) } f() } }\n";
        assert!(
            !has(src, "defer-in-loop"),
            "nested arrow inside loop should NOT fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_is_verbatim() {
        let src = "fn f() { while (true) { defer print(1) } }\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-in-loop")
            .unwrap();
        assert_eq!(
            d.message,
            "'defer' inside a loop registers one call per iteration; they all run at function exit — wrap the loop body in a function if you want per-iteration cleanup"
        );
    }
}
