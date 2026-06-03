//! `undefined-variable`: a NameRef the resolver classifies `Global` whose name
//! is neither a builtin, nor `self`/`super`, nor an imported/local binding.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{BindingKind, Resolution, ResolveResult};
use std::collections::HashSet;

/// Names always available even though the resolver marks them Global.
fn is_allowed_global(name: &str) -> bool {
    name == "self" || name == "super" || crate::interp::BUILTIN_NAMES.contains(&name)
}

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    // Hoistable declarations (fn/class/enum) are visible before their textual
    // position; the resolver records pre-declaration uses as Global. Treat any
    // Global whose name matches such a declaration as defined (conservative — we
    // never flag a name that IS declared somewhere as a hoistable item).
    //
    // MODULE-SCOPE USER-GLOBALS (every DIRECT-child top-level `let`/`const`/`fn`/
    // `class`/`enum`/`import`) resolve to `Global(name)` too, but they ARE defined —
    // a top-level binding's references late-bind to the module global, so exempt any
    // `is_global` binding's name as well.
    let hoisted: HashSet<&str> = resolved
        .bindings
        .iter()
        .filter(|b| {
            b.is_global || matches!(b.kind, BindingKind::Fn | BindingKind::Class | BindingKind::Enum)
        })
        .map(|b| b.name.as_str())
        .collect();
    for n in tree.descendants().filter(|n| n.kind() == SyntaxKind::NameRef) {
        if let Some(Resolution::Global(name)) = resolved.uses.get(&n.text_range()) {
            if !is_allowed_global(name) && !hoisted.contains(name.as_str()) {
                out.push(AsDiagnostic {
                    range: crate::check::rules::code_range(n),
                    severity: Severity::Warning,
                    code: "undefined-variable".to_string(),
                    message: format!("`{name}` is not defined"),
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

    fn codes(src: &str) -> Vec<String> {
        analyze(src).diagnostics.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn flags_genuinely_undefined() {
        assert!(codes("print(nope)\n").contains(&"undefined-variable".to_string()));
    }

    #[test]
    fn does_not_flag_builtins_locals_or_imports() {
        // print/len are builtins; x is local; t is an imported alias used.
        let src = "import * as t from \"std/task\"\nlet x = 1\nprint(len([x]))\nt.spawn\n";
        assert!(
            !codes(src).contains(&"undefined-variable".to_string()),
            "no undefined-variable expected: {:?}",
            codes(src)
        );
    }

    #[test]
    fn does_not_flag_self_super() {
        // inside a method `self`/`super` are allowed even though Global.
        let src = "class C {\n  fn f() { return self }\n}\n";
        assert!(!codes(src).contains(&"undefined-variable".to_string()));
    }
}
