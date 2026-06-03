//! `shadowing` (Hint): a binding whose name shadows an enclosing binding.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(_tree: &ResolvedNode, resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    resolved
        .bindings
        .iter()
        .filter(|b| b.shadows.is_some())
        .map(|b| AsDiagnostic {
            range: ByteSpan::from(b.decl_range),
            severity: Severity::Hint,
            code: "shadowing".to_string(),
            message: format!("`{}` shadows an outer binding", b.name),
            fix: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_shadowing() {
        // inner `x` shadows outer `x`
        assert!(has(
            "let x = 1\n{ let x = 2\n print(x) }\nprint(x)\n",
            "shadowing"
        ));
    }
    #[test]
    fn no_shadow_no_flag() {
        assert!(!has(
            "let x = 1\nlet y = 2\nprint(x)\nprint(y)\n",
            "shadowing"
        ));
    }
}
