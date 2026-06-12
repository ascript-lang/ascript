//! `defer-async-call` (Warning, default-on, §6.2): fires on a **bare** (non-`await`)
//! `defer` whose callee is a bare identifier that resolves, within this file, to a
//! syntactically `async fn` declaration — provably the §3.4 runtime panic (deferred
//! call returns a `future<T>` that is dropped, not awaited).
//!
//! Zero-FP by construction:
//! - Only a PLAIN IDENTIFIER callee is inspected (not `obj.method`, not an imported
//!   name, not a closure or dynamic callee — the runtime error is the backstop for
//!   those).
//! - The callee's RESOLVED binding is matched to the specific `FnDecl` AST node by
//!   `decl_range`, and THAT node is checked for the `async` keyword — so a shadowing
//!   non-async local fn that resolves to a different binding does NOT fire even if an
//!   outer `async fn` shares the name.
//! - Does NOT fire on `defer await fn(...)` (the awaited form is correct and safe).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{code_range, fn_name};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution, ResolveResult};
use std::collections::HashMap;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    // Build a map from decl_range → fn name, restricted to ASYNC fn declarations.
    // The resolver records `decl_range = node.text_range()` for `FnDecl` bindings
    // (see `src/syntax/resolve/mod.rs` FnDecl arm), so we key by the full node range.
    let async_fn_by_range: HashMap<cstree::text::TextRange, String> = tree
        .descendants()
        .filter(|n| n.kind() == FnDecl && is_async_fn(n))
        .filter_map(|n| {
            let name = fn_name(n)?;
            Some((fn_node_range(n), name))
        })
        .collect();

    if async_fn_by_range.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for defer in tree.descendants().filter(|n| n.kind() == DeferStmt) {
        // Skip `defer await ...` — the awaited form is correct.
        if has_await_modifier(defer) {
            continue;
        }

        // The deferred call must be a CallExpr whose callee is a bare NameRef.
        let Some(call) = defer.children().find(|c| c.kind() == CallExpr) else {
            continue;
        };
        let Some(callee) = call.children().find(|c| c.kind() == NameRef) else {
            // Not a bare name (member call, dynamic, etc.) — out of scope.
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(callee) else {
            continue;
        };

        // Look up what the callee resolves to.
        let resolution = resolved.uses.get(&callee.text_range());

        // Find the resolved binding and get its `decl_range`.
        let decl_range = match resolution {
            Some(Resolution::Local(slot)) | Some(Resolution::Upvalue(slot)) => {
                // Local or upvalue: find the binding with this slot in the bindings vec.
                resolved
                    .bindings
                    .iter()
                    .find(|b| {
                        !b.is_global && b.slot == *slot && b.name == name && b.kind == BindingKind::Fn
                    })
                    .map(|b| b.decl_range)
            }
            Some(Resolution::Global(gname)) => {
                // Module-scope global: find the global fn binding with this name.
                resolved
                    .bindings
                    .iter()
                    .find(|b| b.is_global && b.name == *gname && b.kind == BindingKind::Fn)
                    .map(|b| b.decl_range)
            }
            _ => None,
        };

        let Some(decl_range) = decl_range else {
            continue;
        };

        // The resolved binding's decl_range must match one of our async fn declarations.
        if let Some(async_name) = async_fn_by_range.get(&decl_range) {
            out.push(AsDiagnostic {
                range: code_range(defer),
                severity: Severity::Warning,
                code: "defer-async-call".to_string(),
                message: format!(
                    "deferred call to async fn '{async_name}' will panic at runtime — use 'defer await {async_name}(…)'"
                ),
                fix: None,
            });
        }
    }
    out
}

/// True if the `FnDecl` node has an `async` keyword token (i.e. is `async fn`).
fn is_async_fn(fn_decl: &ResolvedNode) -> bool {
    fn_decl
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AsyncKw)
}

/// True if the `DeferStmt` node has an `await` keyword token
/// (i.e. it is `defer await ...`).
fn has_await_modifier(defer_stmt: &ResolvedNode) -> bool {
    defer_stmt
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::AwaitKw)
}

/// The `TextRange` of the whole `FnDecl` node — the same range the resolver
/// records as `decl_range` for a `Fn` binding (`node.text_range()`).
fn fn_node_range(fn_decl: &ResolvedNode) -> cstree::text::TextRange {
    fn_decl.text_range()
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
    fn fires_on_bare_defer_to_async_fn() {
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        assert!(
            has(src, "defer-async-call"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_on_defer_await_async_fn() {
        // `defer await` is the correct form — must NOT fire.
        let src = "async fn teardown() { }\nasync fn main() { defer await teardown() }\n";
        assert!(
            !has(src, "defer-async-call"),
            "defer await should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_on_member_callee() {
        // `defer obj.m()` — member callee is out of scope.
        let src = "fn main() { let r = {} defer r.teardown() }\n";
        assert!(
            !has(src, "defer-async-call"),
            "member callee should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_on_non_async_fn() {
        // Plain (non-async) fn — must not fire.
        let src = "fn cleanup() { }\nfn main() { defer cleanup() }\n";
        assert!(
            !has(src, "defer-async-call"),
            "non-async fn should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn no_fire_when_name_shadows_async_fn() {
        // The local non-async fn shadows the outer async fn.
        // The callee resolves to the LOCAL fn, which is NOT async → no fire.
        let src = "async fn teardown() { }\nfn main() { fn teardown() { } defer teardown() }\n";
        assert!(
            !has(src, "defer-async-call"),
            "shadowed name should not fire: {:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_contains_name() {
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-async-call")
            .unwrap();
        assert!(
            d.message.contains("teardown"),
            "message should contain fn name: {d:?}"
        );
        assert!(
            d.message.contains("defer await teardown"),
            "message should contain fix suggestion: {d:?}"
        );
    }

    #[test]
    fn message_is_verbatim() {
        let src = "async fn teardown() { }\nfn main() { defer teardown() }\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "defer-async-call")
            .unwrap();
        assert_eq!(
            d.message,
            "deferred call to async fn 'teardown' will panic at runtime — use 'defer await teardown(…)'"
        );
    }
}
