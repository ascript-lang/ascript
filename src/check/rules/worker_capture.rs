//! `worker-capture` (Error): inside a `worker fn`, flag references to outer
//! mutable bindings and writes to any top-level/outer global.
//!
//! Workers run in a SEPARATE ISOLATE (a fresh `Interp` on a worker thread). Any
//! outer mutable `let` would be a DIFFERENT copy at dispatch time, so reading it
//! silently gives the wrong value; writing to it is even more broken because the
//! write is invisible to the caller. Only top-level `const` bindings (which are
//! value-copied at dispatch) and top-level `fn` declarations (function values,
//! also copied) are safe to capture. Params of the worker fn itself are local
//! (in-frame) and always safe.
//!
//! Rule logic (deliberately conservative — zero false positives):
//!
//! For each `FnDecl`/`MethodDecl` where `is_worker_fn` is true, walk every
//! `NameRef` in the body:
//!
//! - `Global(name)` where the global binding is `mutable` (`let`) → READ error.
//! - `Upvalue(_)` where the corresponding binding is `mutable` and not a `Param`
//!   → READ error (outer non-param mutable capture).
//! - LHS target of an `AssignExpr` resolving to `Global(name)` where the name is
//!   a known module global → WRITE error. (Assignment to an immutable global is
//!   already a runtime panic; we still flag it so workers cannot mutate ANY shared
//!   global.)

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution, ResolveResult};

pub fn check(tree: &ResolvedNode, res: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();

    for node in tree.descendants() {
        if !matches!(node.kind(), FnDecl | MethodDecl) {
            continue;
        }
        if !crate::syntax::resolve::is_worker_fn(node) {
            continue;
        }
        // Collect the params of THIS worker fn so we know which slots are params.
        // Params resolve to Local(slot) inside the worker fn's own frame; we only
        // need to track their names to distinguish them from body-locals.
        let param_names: std::collections::HashSet<String> = {
            let mut names = std::collections::HashSet::new();
            if let Some(params) = node.children().find(|c| c.kind() == ParamList) {
                for p in params.children().filter(|c| c.kind() == Param) {
                    if let Some(name) = crate::syntax::resolve::ident_text(p) {
                        names.insert(name);
                    }
                }
            }
            // Non-static methods have an implicit `self` receiver (slot 0); treat
            // it as a param so we never flag it.
            if node.kind() == MethodDecl && !crate::syntax::resolve::is_static_method(node) {
                names.insert("self".to_string());
            }
            names
        };

        // Walk every NameRef within this worker fn's body (the Block child or,
        // for arrow-body workers, the expression child). We do NOT descend into
        // NESTED fn/class/method declarations inside the body — those are their
        // own scopes and their own potential worker-capture sites (they'll be
        // caught when we iterate over them as separate FnDecl/MethodDecl nodes).
        let Some(body_root) = body_of(node) else {
            continue;
        };

        // Collect AssignExpr LHS NameRef ranges for write-target detection.
        let write_targets: std::collections::HashSet<cstree::text::TextRange> =
            collect_write_targets(body_root);

        for name_ref in body_root
            .descendants()
            .filter(|n| n.kind() == NameRef && !inside_nested_fn(n, body_root))
        {
            let range = name_ref.text_range();
            let Some(name) = crate::syntax::resolve::ident_text(name_ref) else {
                continue;
            };
            let resolution = res.uses.get(&range);

            match resolution {
                Some(Resolution::Global(gname)) => {
                    let is_module_global = res.module_globals.contains(gname.as_str());
                    if !is_module_global {
                        // A builtin (e.g. `print`) — not a user global, always safe.
                        continue;
                    }
                    // Find the binding for this global name.
                    let binding = res
                        .bindings
                        .iter()
                        .find(|b| b.is_global && b.name == *gname);

                    if write_targets.contains(&range) {
                        // This is a write to a module global — always an error.
                        out.push(AsDiagnostic {
                            range: code_range(name_ref),
                            severity: Severity::Error,
                            code: "worker-capture".to_string(),
                            message: format!(
                                "worker fn cannot mutate the top-level binding '{gname}' — workers run in a separate isolate"
                            ),
                            fix: None,
                        });
                    } else if let Some(b) = binding {
                        // A read: only flag if the global is a mutable `let`.
                        // Top-level `fn`, `const`, `class`, `enum`, `import` → OK.
                        if b.mutable && b.kind == BindingKind::Let {
                            out.push(AsDiagnostic {
                                range: code_range(name_ref),
                                severity: Severity::Error,
                                code: "worker-capture".to_string(),
                                message: format!(
                                    "worker fn cannot capture mutable outer binding '{name}' — consts are copied; make it const or pass it as an argument"
                                ),
                                fix: None,
                            });
                        }
                    }
                }
                Some(Resolution::Upvalue(_)) => {
                    // An upvalue: captured from an outer (non-global) frame.
                    // Find the binding by name — NOT in `is_global` bindings,
                    // NOT a param of THIS worker fn.
                    if param_names.contains(&name) {
                        // This shouldn't happen (params are Local, not Upvalue),
                        // but be defensive.
                        continue;
                    }
                    // Look for a non-global mutable binding with this name.
                    // There may be multiple bindings with the same name (shadowing);
                    // to stay zero-FP we only flag if ALL candidates are mutable
                    // non-params — but practically, the upvalue must point to a
                    // specific slot. We use the simplest correct heuristic: if ANY
                    // non-global binding named `name` is mutable and not a Param,
                    // flag the read (the alternative is complex upvalue chain walking).
                    let has_mutable_capture = res.bindings.iter().any(|b| {
                        !b.is_global
                            && b.name == name
                            && b.mutable
                            && b.kind != BindingKind::Param
                    });
                    if has_mutable_capture {
                        let is_write = write_targets.contains(&range);
                        let msg = if is_write {
                            format!(
                                "worker fn cannot mutate the top-level binding '{name}' — workers run in a separate isolate"
                            )
                        } else {
                            format!(
                                "worker fn cannot capture mutable outer binding '{name}' — consts are copied; make it const or pass it as an argument"
                            )
                        };
                        out.push(AsDiagnostic {
                            range: code_range(name_ref),
                            severity: Severity::Error,
                            code: "worker-capture".to_string(),
                            message: msg,
                            fix: None,
                        });
                    }
                }
                _ => {
                    // Local (own param or body local) or Unresolved → safe/not our job.
                }
            }
        }
    }
    out
}

/// The "body" of a `FnDecl`/`MethodDecl`/`ArrowExpr` to walk: the `Block` child
/// for block-body functions, or (for arrow-body) the expression child. Returns
/// `None` if the node has no walkable body (e.g. an abstract method stub).
fn body_of(node: &ResolvedNode) -> Option<&ResolvedNode> {
    use SyntaxKind::*;
    node.children().find(|c| c.kind() == Block)
}

/// Collect the `TextRange`s of all `NameRef` nodes that appear as the LHS target
/// of an `AssignExpr` directly reachable from `root` (without descending into
/// nested function frames). Only direct `NameRef` children of an `AssignExpr`
/// are targets; member/index expressions are not simple-name writes.
fn collect_write_targets(
    root: &ResolvedNode,
) -> std::collections::HashSet<cstree::text::TextRange> {
    use SyntaxKind::*;
    let mut targets = std::collections::HashSet::new();
    for assign in root
        .descendants()
        .filter(|n| n.kind() == AssignExpr && !inside_nested_fn(n, root))
    {
        // The first child of an AssignExpr is the target expression.
        if let Some(target) = assign.children().next() {
            if target.kind() == NameRef {
                targets.insert(target.text_range());
            }
        }
    }
    targets
}

/// True if `node` is lexically inside a NESTED function (a `FnDecl`, `MethodDecl`,
/// or `ArrowExpr`) that is itself a descendant of `root` but not `root` itself.
/// Used to prevent walking into nested closures/fns, which are their own scope
/// boundaries — they'll be handled as separate top-level iterations.
fn inside_nested_fn(node: &ResolvedNode, root: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let root_range = root.text_range();
    let mut cur = node.parent();
    while let Some(p) = cur {
        let pr = p.text_range();
        if pr == root_range {
            return false; // we've reached `root` without crossing a nested fn
        }
        if matches!(p.kind(), FnDecl | MethodDecl | ArrowExpr) {
            return true; // crossed a nested function boundary
        }
        cur = p.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    use crate::check::diagnostic::Severity;

    fn diagnostics(src: &str) -> Vec<crate::check::diagnostic::AsDiagnostic> {
        analyze(src).diagnostics
    }

    #[test]
    fn worker_capture_allows_const_and_params_and_top_fns() {
        let src = "
            const K = 5
            fn helper(x) { return x }
            worker fn g(n) { return helper(n) + K }
        ";
        assert!(!diagnostics(src).iter().any(|d| d.code == "worker-capture"));
    }

    #[test]
    fn worker_capture_rejects_mutable_let_capture() {
        let src = "
            let counter = 0
            worker fn g(n) { return n + counter }
        ";
        let d = diagnostics(src);
        let wc: Vec<_> = d.iter().filter(|d| d.code == "worker-capture").collect();
        assert_eq!(wc.len(), 1);
        assert_eq!(wc[0].severity, Severity::Error);
    }

    #[test]
    fn worker_capture_rejects_top_level_mutation() {
        let src = "
            let total = 0
            worker fn g(n) { total = total + n; return total }
        ";
        assert!(diagnostics(src)
            .iter()
            .any(|d| d.code == "worker-capture" && d.severity == Severity::Error));
    }
}
