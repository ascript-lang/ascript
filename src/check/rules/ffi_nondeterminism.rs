//! `ffi-nondeterminism` (Warning, FFI §7): inside a function passed to
//! `workflow.run`/`workflow.resume`, flag DIRECT `ffi.*` calls (`ffi.open`,
//! `ffi.cstr`, `ffi.read_cstr`, `ffi.struct`, …) — recommending the `activity` form,
//! because a foreign call is an opaque effect seam: a **pointer-returning** call or a
//! **`ForeignPtr` out-param** is NOT replayable (a runtime Tier-2 refusal, §7B), and
//! even a recordable call should be driven through an `activity` so its boundary is
//! event-sourced (recorded once at Record, replayed without re-invoking C) rather
//! than relying on the implicit per-call record.
//!
//! BEST-EFFORT and ZERO-FALSE-POSITIVE on the corpus (the checker bar): it mirrors
//! `workflow-determinism` exactly —
//! - it only inspects the workflow function when it is passed INLINE as an arrow
//!   expression to `run`/`resume` (a named function passed by reference is the
//!   documented best-effort limit; AScript has no `fn`-expression form);
//! - a flagged call must be a DIRECT `ffi.<member>` call, NOT wrapped in an
//!   `activity(...)` (that is the correct, recorded form).
//!
//! The `ffi` namespace name is matched textually (the `ffi.*` module is imported as a
//! namespace, `import * as ffi from "std/ffi"`), so it does not fire on an unrelated
//! local named `ffi` calling a non-FFI method only if that local shadows the import —
//! an accepted, vanishingly-rare best-effort edge, same as `workflow-determinism`.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        if !is_workflow_driver(call) {
            continue;
        }
        let Some(args) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        let Some(wf) = args.children().find(|c| c.kind() == ArrowExpr) else {
            continue;
        };
        flag_ffi_in(wf, &mut out);
    }
    out
}

/// True if `call`'s callee resolves to `run`/`resume` (bare) or `<ns>.run`/`.resume`.
fn is_workflow_driver(call: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    if let Some(name) = call.children().find(|c| c.kind() == NameRef) {
        if let Some(t) = crate::syntax::resolve::ident_text(name) {
            return t == "run" || t == "resume";
        }
    }
    if let Some(member) = call.children().find(|c| c.kind() == MemberExpr) {
        let prop = member_property(member);
        return prop.as_deref() == Some("run") || prop.as_deref() == Some("resume");
    }
    false
}

/// The property name of a `MemberExpr` `recv.name` — the LAST `Ident` token under the
/// member (mirrors `workflow_determinism::member_property`).
fn member_property(member: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let idents: Vec<_> = member
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == Ident)
        .collect();
    idents.last().map(|t| t.text().to_string())
}

/// The object (namespace) name of a `MemberExpr` whose object is a bare name
/// (`ffi.open` → `"ffi"`). `None` if the object is not a simple name.
fn member_object_name(member: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let obj = member.children().find(|c| !matches!(c.kind(), Dot))?;
    if obj.kind() == NameRef {
        crate::syntax::resolve::ident_text(obj)
    } else {
        None
    }
}

/// Walk the workflow body and flag direct `ffi.<member>` calls NOT inside an
/// `activity(...)` argument.
fn flag_ffi_in(wf: &ResolvedNode, out: &mut Vec<AsDiagnostic>) {
    use SyntaxKind::*;
    for call in wf.descendants().filter(|n| n.kind() == CallExpr) {
        if inside_activity(call) {
            continue;
        }
        let Some(member) = call.children().find(|c| c.kind() == MemberExpr) else {
            continue;
        };
        let (Some(ns), Some(prop)) = (member_object_name(member), member_property(member)) else {
            continue;
        };
        if ns == "ffi" {
            out.push(AsDiagnostic {
                range: code_range(call),
                severity: Severity::Warning,
                code: "ffi-nondeterminism".to_string(),
                message: format!(
                    "`ffi.{prop}` is a foreign-call effect seam; in a workflow, drive native work through an `activity` (via `ctx.call`) — a pointer-returning call or a foreign-pointer out-param is not replayable, and recordable calls should be event-sourced at the activity boundary"
                ),
                fix: None,
            });
        }
    }
}

/// True if `call` is lexically inside an `activity(...)` call's argument list (the
/// correct, recorded form). Mirrors `workflow_determinism::inside_activity`.
fn inside_activity(call: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let mut cur = call.parent();
    while let Some(node) = cur {
        if node.kind() == CallExpr {
            if let Some(name) = node.children().find(|c| c.kind() == NameRef) {
                if crate::syntax::resolve::ident_text(name).as_deref() == Some("activity") {
                    return true;
                }
            }
            if let Some(member) = node.children().find(|c| c.kind() == MemberExpr) {
                if member_property(member).as_deref() == Some("activity") {
                    return true;
                }
            }
        }
        cur = node.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn flags_direct_ffi_open_in_inline_workflow() {
        let src = r#"
import { run } from "std/workflow"
import * as ffi from "std/ffi"
await run((ctx, input) => {
  let [lib, e] = ffi.open("libm.so.6")
  return 0
}, 0, { log: "x" })
"#;
        assert!(has(src, "ffi-nondeterminism"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn ffi_inside_activity_not_flagged() {
        let src = r#"
import { run, activity } from "std/workflow"
import * as ffi from "std/ffi"
let openLib = activity("open", (x) => ffi.open("libm.so.6"))
await run((ctx, input) => {
  return ctx.call(openLib, input)
}, 0, { log: "x" })
"#;
        assert!(!has(src, "ffi-nondeterminism"));
    }

    #[test]
    fn ffi_outside_any_workflow_not_flagged() {
        let src = "import * as ffi from \"std/ffi\"\nlet [lib, e] = ffi.open(\"libm.so.6\")\n";
        assert!(!has(src, "ffi-nondeterminism"));
    }

    /// Zero-FP: a non-ffi namespace call in a workflow body is NOT flagged by this rule.
    #[test]
    fn non_ffi_call_in_workflow_not_flagged_by_this_rule() {
        let src = r#"
import { run } from "std/workflow"
import * as math from "std/math"
await run((ctx, input) => {
  return math.abs(input)
}, 0, { log: "x" })
"#;
        assert!(!has(src, "ffi-nondeterminism"));
    }
}
