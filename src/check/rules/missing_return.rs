//! `missing-return` (conservative): a function with a declared non-`nil` return
//! type whose body can fall off the end without returning. To avoid false
//! positives, a body is considered to return when it DEFINITELY returns (last
//! stmt is `return`, or an if/else where both branches definitely return, or
//! ends in an expression statement that may be the value). Uncertain → silent.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;
    let mut out = Vec::new();
    for f in tree
        .descendants()
        .filter(|n| matches!(n.kind(), FnDecl | MethodDecl))
    {
        // Only check functions with a declared, non-nil return type.
        let Some(rt) = f.children().find(|c| c.kind() == RetType) else {
            continue;
        };
        if returns_nil(rt) {
            continue;
        }
        let Some(body) = f.children().find(|c| c.kind() == Block) else {
            continue;
        };
        if !definitely_returns(body) {
            // point at the function's range for a clear location
            let range = f.text_range();
            out.push(AsDiagnostic {
                range: ByteSpan::from(range),
                severity: Severity::Warning,
                code: "missing-return".to_string(),
                message: "function with a declared return type may not return a value".to_string(),
                fix: None,
            });
        }
    }
    out
}

/// A return type is "nullable" (no value strictly required) when it mentions the
/// `nil` keyword (`: nil`, `: T | nil`) or carries the optional suffix (`: T?`).
fn returns_nil(ret_type: &ResolvedNode) -> bool {
    ret_type
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| matches!(t.kind(), SyntaxKind::NilKw | SyntaxKind::Question))
}

/// Conservative: a block DEFINITELY returns if its last statement is a `return`,
/// or an `if/else` whose both branches definitely return. An ending expression
/// statement is treated as a possible value (no false positive). Everything else
/// (ends in let/while/assignment/for) is treated as NOT definitely returning.
fn definitely_returns(block: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let Some(last) = block.children().filter(|c| is_block_stmt(c.kind())).last() else {
        return false;
    };
    match last.kind() {
        ReturnStmt => true,
        ExprStmt => true, // may be the implicit value; don't flag
        IfStmt => {
            // both then-block and an else-block must definitely return
            let blocks: Vec<_> = last.children().filter(|c| c.kind() == Block).collect();
            let has_else = blocks.len() == 2 || last.children().any(|c| c.kind() == IfStmt);
            if !has_else {
                return false;
            }
            let then_ok = blocks.first().map(|b| definitely_returns(b)).unwrap_or(false);
            let else_ok = if let Some(elif) = last.children().find(|c| c.kind() == IfStmt) {
                // else-if chain: treat as a nested block requirement
                definitely_returns_ifchain(elif)
            } else {
                blocks
                    .get(1)
                    .map(|b| definitely_returns(b))
                    .unwrap_or(false)
            };
            then_ok && else_ok
        }
        Block => definitely_returns(last),
        _ => false,
    }
}

fn definitely_returns_ifchain(if_stmt: &ResolvedNode) -> bool {
    use SyntaxKind::*;
    let blocks: Vec<_> = if_stmt.children().filter(|c| c.kind() == Block).collect();
    let then_ok = blocks.first().map(|b| definitely_returns(b)).unwrap_or(false);
    let else_ok = if let Some(elif) = if_stmt.children().find(|c| c.kind() == IfStmt) {
        definitely_returns_ifchain(elif)
    } else {
        blocks
            .get(1)
            .map(|b| definitely_returns(b))
            .unwrap_or(false)
    };
    then_ok && else_ok
}

fn is_block_stmt(kind: SyntaxKind) -> bool {
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
    )
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn has(src: &str, code: &str) -> bool {
        analyze(src).diagnostics.iter().any(|d| d.code == code)
    }
    #[test]
    fn flags_obvious_missing_return() {
        // declared : number but body ends in a let → cannot return a value
        assert!(has("fn f(): number { let x = 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn no_flag_when_returns() {
        assert!(!has("fn f(): number { return 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn no_flag_if_else_both_return() {
        assert!(!has(
            "fn f(x): number { if x { return 1 } else { return 2 } }\nf(1)\n",
            "missing-return"
        ));
    }
    #[test]
    fn no_flag_when_ends_in_expression() {
        // ends in a match/expr statement — treated as a possible value (no FP)
        assert!(!has("fn f(x): number { match x { _ => 0 } }\nf(1)\n", "missing-return"));
    }
    #[test]
    fn nullable_return_exempt() {
        // `?` suffix makes nil acceptable, so falling off the end is fine.
        assert!(!has("fn f(): number? { let x = 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn nil_return_exempt() {
        // `: nil` requires no value.
        assert!(!has("fn f(): nil { let x = 1 }\nf()\n", "missing-return"));
    }
    #[test]
    fn substring_type_not_exempt() {
        // a plain non-nullable type (no NilKw / Question token) still flags when
        // the body cannot return a value.
        assert!(has("fn f(): number { let x = 1 }\nf()\n", "missing-return"));
    }
}
