//! `workflow-determinism` (Warning, SP9 §2.5): inside a function passed to
//! `workflow.run`/`workflow.resume`, flag DIRECT calls to known non-deterministic
//! stdlib seams (`time.now`, `date.now`, `math.random`, `crypto.randomBytes`,
//! `uuid.v4`, and `net.*`/`fs.*`/`sql.*`) — recommending the `ctx`/activity form, so
//! the workflow stays deterministic across replay.
//!
//! BEST-EFFORT and ZERO-FALSE-POSITIVE on the corpus (the existing checker bar): the
//! *runtime* replay-mismatch detector (`ctx.call`) is the authoritative guarantee.
//! To stay zero-FP this rule is deliberately narrow:
//! - it only inspects the workflow function when it is passed INLINE as an arrow or
//!   `fn` expression to `run`/`resume` (a named function passed by reference is not
//!   followed — that is the documented best-effort limit);
//! - a flagged call must be a DIRECT module member call (`time.now()`), not anything
//!   wrapped in an `activity(...)` (those are the correct, recorded form), and not a
//!   `ctx.*` method.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

/// Non-deterministic `module.func` seams that must go through an activity / `ctx`.
const SEAM_CALLS: &[(&str, &str)] = &[
    ("time", "now"),
    ("time", "monotonic"),
    ("date", "now"),
    ("math", "random"),
    ("math", "randomInt"),
    ("math", "shuffle"),
    ("math", "sample"),
    ("crypto", "randomBytes"),
    ("uuid", "v4"),
    ("uuid", "v7"),
];
/// Whole modules whose every call is non-deterministic I/O.
const SEAM_MODULES: &[&str] = &["net", "fs", "sql", "process", "http"];

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for call in tree.descendants().filter(|n| n.kind() == CallExpr) {
        // The callee must be `run`/`resume` — either a bare name (imported) or a
        // `workflow.run` member.
        if !is_workflow_driver(call) {
            continue;
        }
        let Some(args) = call.children().find(|c| c.kind() == ArgList) else {
            continue;
        };
        // The first arg is the workflow function. Only follow it when it is INLINE
        // (arrow or fn expression) — a named reference is the best-effort limit.
        let Some(wf) = args
            .children()
            .find(|c| c.kind() == ArrowExpr)
        else {
            continue;
        };
        flag_seams_in(wf, &mut out);
    }
    out
}

/// True if `call`'s callee resolves to `run`/`resume` (bare) or `<ns>.run`/`.resume`.
fn is_workflow_driver(call: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    // Bare name: `run(...)` / `resume(...)`.
    if let Some(name) = call.children().find(|c| c.kind() == NameRef) {
        if let Some(t) = crate::syntax::resolve::ident_text(name) {
            return t == "run" || t == "resume";
        }
    }
    // Member: `workflow.run(...)`.
    if let Some(member) = call.children().find(|c| c.kind() == MemberExpr) {
        // The member's property name is the last NameRef/Name token in the member.
        let prop = member_property(member);
        return prop.as_deref() == Some("run") || prop.as_deref() == Some("resume");
    }
    false
}

/// The property name of a `MemberExpr` `recv.name` — the LAST `Ident` TOKEN under
/// the member (after the `.`). The property is a bare token, NOT a NameRef node, so
/// it is read from `children_with_tokens` (mirrors `call_arity::member_property_name`).
fn member_property(member: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let idents: Vec<_> = member
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == Ident)
        .collect();
    idents.last().map(|t| t.text().to_string())
}

/// Walk the workflow body and flag direct seam calls that are NOT inside an
/// `activity(...)` argument.
fn flag_seams_in(wf: &ResolvedNode, out: &mut Vec<AsDiagnostic>) {
    use SyntaxKind::*;
    for call in wf.descendants().filter(|n| n.kind() == CallExpr) {
        // Skip calls that are lexically inside an `activity(...)` call (the correct,
        // recorded form). We detect this by walking ancestors for an activity call.
        if inside_activity(call) {
            continue;
        }
        let Some(member) = call.children().find(|c| c.kind() == MemberExpr) else {
            continue;
        };
        let (Some(ns), Some(prop)) = (member_object_name(member), member_property(member)) else {
            continue;
        };
        let is_seam = SEAM_MODULES.contains(&ns.as_str())
            || SEAM_CALLS.iter().any(|(m, f)| *m == ns && *f == prop);
        if is_seam {
            out.push(AsDiagnostic {
                range: code_range(call),
                severity: Severity::Warning,
                code: "workflow-determinism".to_string(),
                message: format!(
                    "`{ns}.{prop}` is non-deterministic; in a workflow, call it inside an `activity` (via `ctx.call`) so replay stays deterministic"
                ),
                fix: None,
            });
        }
    }
}

/// The object (namespace) name of a `MemberExpr` whose object is a bare name
/// (`time.now` → `"time"`). `None` if the object is not a simple name.
fn member_object_name(member: &ResolvedNode) -> Option<String> {
    use SyntaxKind::*;
    let obj = member.children().find(|c| !matches!(c.kind(), Dot))?;
    if obj.kind() == NameRef {
        crate::syntax::resolve::ident_text(obj)
    } else {
        None
    }
}

/// True if `call` is lexically inside an `activity(...)` call's argument list.
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
    fn flags_direct_time_now_in_inline_workflow() {
        let src = r#"
import { run } from "std/workflow"
import { now } from "std/time"
await run((ctx, input) => {
  let t = time.now()
  return t
}, 0, { log: "x" })
"#;
        assert!(
            has(src, "workflow-determinism"),
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn correct_ctx_now_not_flagged() {
        let src = r#"
import { run } from "std/workflow"
await run((ctx, input) => {
  let t = ctx.now()
  return t
}, 0, { log: "x" })
"#;
        assert!(!has(src, "workflow-determinism"));
    }

    #[test]
    fn seam_inside_activity_not_flagged() {
        let src = r#"
import { run, activity } from "std/workflow"
let stamp = activity("stamp", (x) => time.now())
await run((ctx, input) => {
  return ctx.call(stamp, input)
}, 0, { log: "x" })
"#;
        assert!(!has(src, "workflow-determinism"));
    }

    #[test]
    fn time_now_outside_any_workflow_not_flagged() {
        let src = "import { now } from \"std/time\"\nlet t = time.now()\n";
        assert!(!has(src, "workflow-determinism"));
    }
}

