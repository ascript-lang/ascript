//! `unused-binding` / `unused-import`: a binding with zero read uses. Parameters
//! are exempt (often intentionally unused). Imports/lets get a removal fix.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::{Binding, BindingKind, ResolveResult};

pub fn check(_tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for b in &resolved.bindings {
        if b.use_count != 0 {
            continue;
        }
        // An underscore-prefixed name (incl. bare `_`) is the conventional
        // "intentionally unused" marker — never flag it.
        if b.name.starts_with('_') {
            continue;
        }
        match b.kind {
            BindingKind::Param => {} // params are exempt
            BindingKind::Import => out.push(unused(b, "unused-import", "remove unused import")),
            BindingKind::Let | BindingKind::Const => {
                out.push(unused(b, "unused-binding", "remove unused binding"))
            }
            // PatternBind is exempt: destructuring a `[value, err]` Result pair (or
            // an object/array) idiomatically binds slots that aren't all read.
            // fn/class/enum/loop-var: skip (often public API / loop counters).
            _ => {}
        }
    }
    out
}

fn unused(b: &Binding, code: &str, fix_title: &str) -> AsDiagnostic {
    let range = ByteSpan::from(b.decl_range);
    AsDiagnostic {
        range,
        severity: Severity::Warning,
        code: code.to_string(),
        message: format!("`{}` is never used", b.name),
        fix: Some(Fix {
            title: fix_title.to_string(),
            edits: vec![TextEdit {
                range,
                replacement: String::new(),
            }],
        }),
    }
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn codes(src: &str) -> Vec<String> {
        analyze(src)
            .diagnostics
            .into_iter()
            .map(|d| d.code)
            .collect()
    }
    #[test]
    fn flags_unused_let() {
        assert!(codes("let x = 1\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn used_let_not_flagged() {
        assert!(!codes("let x = 1\nprint(x)\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn flags_unused_import() {
        assert!(codes("import * as t from \"std/task\"\nprint(1)\n")
            .contains(&"unused-import".to_string()));
    }
    #[test]
    fn unused_param_is_exempt() {
        assert!(!codes("fn f(a) { return 1 }\nf(0)\n").contains(&"unused-binding".to_string()));
    }
}
