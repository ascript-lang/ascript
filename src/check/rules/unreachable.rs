//! `unreachable-code`: statements following a `return`/`break`/`continue` in the
//! same block can never execute.

use crate::check::diagnostic::{AsDiagnostic, Severity};
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
                    range: crate::check::rules::code_range(first_dead),
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

    /// Line attribution: the diagnostic must point at the dead statement itself,
    /// NOT at the end of the previous (terminator) line. A CST node's range
    /// includes its leading trivia (the newline/indent before it), so without
    /// `code_range` the diagnostic's `range.start` lands on the `return 1` line.
    #[test]
    fn points_at_dead_statement_not_previous_line() {
        let src = "fn f() {\n  return 1\n  print(2)\n}\nf()\n";
        let diag = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "unreachable-code")
            .expect("expected an unreachable-code diagnostic");
        let print_off = src.find("print(2)").unwrap();
        // The diagnostic must start exactly at (or after) `print(2)`, never on
        // the preceding `return 1` line / its trailing newline.
        assert!(
            diag.range.start >= print_off,
            "diagnostic start {} should be at/after `print(2)` offset {} (would be on the \
             previous line if it included leading trivia)",
            diag.range.start,
            print_off
        );
        // And precisely at the `print` token, not somewhere later.
        assert_eq!(diag.range.start, print_off, "should point at `print`");
    }

    /// With correct line attribution, an inline `ascript-ignore` on the dead line
    /// suppresses the diagnostic. With the off-by-one-line bug the diagnostic
    /// would be attributed to the `return 1` line and the suppression would miss.
    #[test]
    fn inline_suppression_on_dead_line_works() {
        let src = "fn f() {\n  return 1\n  print(2) // ascript-ignore[unreachable-code]\n}\nf()\n";
        assert!(
            !has(src, "unreachable-code"),
            "inline ascript-ignore on the dead-code line should suppress it"
        );
    }
}
