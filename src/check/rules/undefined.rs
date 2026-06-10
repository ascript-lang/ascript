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

/// The closest known name to a typo'd `name` within edit distance, or `None`.
/// Candidate set = every resolver binding name (locals/params/imports/top-level
/// globals) plus `BUILTIN_NAMES`. Order is deterministic (binding declaration
/// order, then builtins), so `suggest::closest`'s stable tie-break is stable here
/// too. Returns an owned `String` so the diagnostic can carry it.
fn closest_name(name: &str, resolved: &ResolveResult) -> Option<String> {
    // Deduplicate while preserving first-seen order so a shadowed/repeated name
    // does not perturb the candidate ordering (determinism).
    let mut seen: HashSet<&str> = HashSet::new();
    let mut candidates: Vec<&str> = Vec::new();
    for b in &resolved.bindings {
        if seen.insert(b.name.as_str()) {
            candidates.push(b.name.as_str());
        }
    }
    for &b in crate::interp::BUILTIN_NAMES {
        if seen.insert(b) {
            candidates.push(b);
        }
    }
    crate::check::suggest::closest(name, candidates.iter().copied()).map(str::to_string)
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
                    // Hoisted/late-bound decls: a reference resolves to the binding even
                    // before its textual declaration. An interface name late-binds via its
                    // `def_env` exactly like a class/enum, so exempt it explicitly rather
                    // than relying on `is_global` (which a nested interface lacks).
                    BindingKind::Fn
                        | BindingKind::Class
                        | BindingKind::Enum
                        | BindingKind::Interface
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
                // DX D4 §5.2 "did you mean": suggest the closest in-scope binding /
                // builtin within edit distance. Candidate set = every resolver
                // binding name (locals, params, imports, top-level globals) +
                // `BUILTIN_NAMES`. The suggestion is appended to the message (the CLI
                // renders it as a `help` note) and carried as a `Fix` so the LSP can
                // offer a one-shot replacement quickfix — but `undefined-variable` is
                // NOT in `FIXABLE_CODES`, so `--fix`/`fixAll` never auto-applies a
                // guess.
                let range = crate::check::rules::code_range(n);
                let suggestion = closest_name(name, resolved);
                let message = match &suggestion {
                    Some(s) => format!("`{name}` is not defined — did you mean `{s}`?"),
                    None => format!("`{name}` is not defined"),
                };
                let fix = suggestion.map(|s| crate::check::diagnostic::Fix {
                    title: format!("Change `{name}` to `{s}`"),
                    edits: vec![crate::check::diagnostic::TextEdit {
                        range,
                        replacement: s,
                    }],
                });
                out.push(AsDiagnostic {
                    range,
                    severity: Severity::Warning,
                    code: "undefined-variable".to_string(),
                    message,
                    fix,
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
        // NUM §4: `int`/`float` are now real conversion BUILTINS, so using `int` as
        // an ordinary value (e.g. `print(int)`) is NOT undefined — it is the builtin.
        assert!(!codes("print(int)\n").contains(&"undefined-variable".to_string()));
        assert!(!codes("print(float)\n").contains(&"undefined-variable".to_string()));
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

    fn message_for(src: &str) -> String {
        analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "undefined-variable")
            .map(|d| d.message)
            .unwrap_or_default()
    }

    #[test]
    fn suggests_closest_builtin() {
        // `lenght` is a typo of the builtin `len` (distance 3 — beyond) BUT also of
        // nothing closer; use a clearer case: `prnt` → `print`.
        let m = message_for("prnt([1])\n");
        assert!(m.contains("did you mean `print`?"), "got: {m}");
    }

    #[test]
    fn suggests_closest_local_binding() {
        // A typo of a local `let` binding suggests it.
        let src = "let length = 5\nprint(lenght)\n";
        let m = message_for(src);
        assert!(m.contains("did you mean `length`?"), "got: {m}");
    }

    #[test]
    fn no_suggestion_beyond_distance() {
        // `zzzzz` is far from every binding/builtin → no "did you mean".
        let m = message_for("print(zzzzz)\n");
        assert!(!m.contains("did you mean"), "got: {m}");
        assert!(m.contains("is not defined"), "got: {m}");
    }

    #[test]
    fn suggestion_carries_a_fix() {
        let fix = analyze("let length = 5\nprint(lenght)\n")
            .diagnostics
            .into_iter()
            .find(|d| d.code == "undefined-variable")
            .and_then(|d| d.fix);
        let fix = fix.expect("a did-you-mean fix");
        assert_eq!(fix.edits.len(), 1);
        assert_eq!(fix.edits[0].replacement, "length");
    }

    #[test]
    fn does_not_flag_self_super() {
        // inside a method `self`/`super` are allowed even though Global.
        let src = "class C {\n  fn f() { return self }\n}\n";
        assert!(!codes(src).contains(&"undefined-variable".to_string()));
    }
}
