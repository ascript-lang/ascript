//! `undefined-variable`: a NameRef the resolver classifies `Global` whose name
//! is neither a builtin, nor `self`/`super`, nor an imported/local binding.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Resolution, ResolveResult};

/// Names always available even though the resolver marks them Global.
fn is_allowed_global(name: &str) -> bool {
    name == "self" || name == "super" || crate::interp::BUILTIN_NAMES.contains(&name)
}

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for n in tree.descendants().filter(|n| n.kind() == SyntaxKind::NameRef) {
        if let Some(Resolution::Global(name)) = resolved.uses.get(&n.text_range()) {
            if !is_allowed_global(name) {
                out.push(AsDiagnostic {
                    range: ByteSpan::from(n.text_range()),
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
