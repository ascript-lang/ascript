//! `unreachable-code`: statements following a `return`/`break`/`continue` in the
//! same block can never execute.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    // Every block-like node (SourceFile, Block) is a statement sequence.
    for block in tree
        .descendants()
        .filter(|n| matches!(n.kind(), SourceFile | Block))
    {
        let stmts: Vec<_> = block.children().filter(|c| is_stmt(c.kind())).collect();
        if let Some(term_idx) = stmts.iter().position(|s| is_terminator(s)) {
            // statements after the FIRST terminator are unreachable.
            if let Some(first_dead) = stmts.get(term_idx + 1) {
                out.push(AsDiagnostic {
                    range: ByteSpan::from(first_dead.text_range()),
                    severity: Severity::Warning,
                    code: "unreachable-code".to_string(),
                    message: "unreachable code".to_string(),
                    fix: None,
                });
            }
        }
    }
    out
}

fn is_terminator(node: &ResolvedNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::ReturnStmt | SyntaxKind::BreakStmt | SyntaxKind::ContinueStmt
    )
}
fn is_stmt(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        LetStmt
            | ExprStmt
            | Block
            | IfStmt
            | WhileStmt
            | ReturnStmt
            | FnDecl
            | ForStmt
            | BreakStmt
            | ContinueStmt
            | EnumDecl
            | ClassDecl
            | ImportStmt
            | ExportStmt
    )
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_after_return() {
        assert!(has("fn f() { return 1\n print(2) }\nf()\n", "unreachable-code"));
    }
    #[test]
    fn no_unreachable_normal_flow() {
        assert!(!has("fn f() { print(1)\n return 2 }\nf()\n", "unreachable-code"));
    }
}
