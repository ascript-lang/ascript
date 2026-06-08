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

/// Is this `NameRef` the RIGHT-hand operand of an `instanceof` binary expression?
/// (i.e. the `int` in `x instanceof int`). Such a NameRef is a reserved type-name
/// RHS, recognized at the operator site rather than as a value binding (NUM §6).
fn is_instanceof_rhs(node: &ResolvedNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != SyntaxKind::BinaryExpr {
        return false;
    }
    // The parent must carry an `instanceof` operator token.
    let has_instanceof = parent
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::InstanceofKw);
    if !has_instanceof {
        return false;
    }
    // This NameRef must be the SECOND (right) expression operand of the binop.
    let mut expr_children = parent
        .children()
        .filter(|c| crate::check::rules::is_expr_kind(c.kind()));
    let _lhs = expr_children.next();
    matches!(expr_children.next(), Some(rhs) if rhs.text_range() == node.text_range())
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
            b.is_global
                || matches!(
                    b.kind,
                    BindingKind::Fn | BindingKind::Class | BindingKind::Enum
                )
        })
        .map(|b| b.name.as_str())
        .collect();
    for n in tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
    {
        if let Some(Resolution::Global(name)) = resolved.uses.get(&n.text_range()) {
            // NUM §6: `x instanceof int|float|number|string|bool` — the RHS is a
            // reserved scalar TYPE NAME, not a value binding. The resolver still
            // classifies it as a `Global` use, so exempt it explicitly (both engines
            // recognize it at the operator site, never looking up a binding).
            if crate::interp::is_reserved_instanceof_type_name(name) && is_instanceof_rhs(n) {
                continue;
            }
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
        analyze(src)
            .diagnostics
            .into_iter()
            .map(|d| d.code)
            .collect()
    }

    #[test]
    fn flags_genuinely_undefined() {
        assert!(codes("print(nope)\n").contains(&"undefined-variable".to_string()));
    }

    #[test]
    fn does_not_flag_reserved_instanceof_rhs() {
        // NUM §6: `x instanceof int|float|number|string|bool` — the reserved type
        // name on the RHS must NOT be flagged as an undefined variable.
        for name in ["int", "float", "number", "string", "bool"] {
            let src = format!("let x = 1\nprint(x instanceof {name})\n");
            assert!(
                !codes(&src).contains(&"undefined-variable".to_string()),
                "`instanceof {name}` should not flag undefined-variable: {:?}",
                codes(&src)
            );
        }
        // But a NON-reserved bare name used like a value IS still flagged.
        assert!(codes("print(nope)\n").contains(&"undefined-variable".to_string()));
        // And `int` used as an ordinary value (not instanceof RHS) is still flagged.
        assert!(codes("print(int)\n").contains(&"undefined-variable".to_string()));
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
