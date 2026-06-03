//! `range-step` (conservative): flag statically-detectable bad ranges over a
//! `RangeExpr`/`RangePat` whose operands are NUMERIC LITERALS — mirroring the
//! guaranteed runtime Tier-2 panic (spec §3.4) at author-time:
//!
//! - a literal `step` of `0` (or `NaN`/`±Infinity`, which cannot be written as a
//!   bare numeric literal — see below) →
//!   *"step must be a finite, non-zero number"* (matches the runtime panic text).
//! - a literal **direction mismatch** (`start`/`end`/`step` all literals,
//!   `start != end`, `sign(step) != sign(end − start)`) →
//!   *"step <k> moves away from end (<end>); range can never progress"* (matches
//!   the runtime panic text, using the same `format_number` formatting).
//! - **advisory:** a **float** `step` inside a **match pattern** (`RangePat`) →
//!   *"float step in a range pattern may not match exactly; consider a guard"*.
//!   (A correctness hazard unique to the predicate position — exact-equality on a
//!   float stride; §3.8. Float steps in loops/value ranges are fine and NOT
//!   flagged.)
//!
//! Conservative like `contract.rs`: any operand that is not a numeric literal
//! makes the relevant check undecidable, so the node is skipped (no false
//! positives on computed bounds/steps).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::code_range;
use crate::interp::format_number;
use crate::lex_literals::parse_number_text;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    let mut out = Vec::new();
    for node in tree
        .descendants()
        .filter(|n| matches!(n.kind(), RangeExpr | RangePat))
    {
        let is_pattern = node.kind() == RangePat;
        // The Expr children, in source order: `start`, `end`, optional `step`
        // (the 3rd, per `parse_range_step` in `src/syntax/parser.rs` — see the
        // compiler's `compile_range_pattern`/`compile_range`).
        let exprs: Vec<ResolvedNode> = node
            .children()
            .filter(|c| is_expr_kind(c.kind()))
            .cloned()
            .collect();
        let (Some(start_n), Some(end_n)) = (exprs.first(), exprs.get(1)) else {
            continue; // malformed; not our job
        };
        let step_n = exprs.get(2);

        // Only a node with an explicit `step` clause can be a bad step / mismatch
        // (an omitted step always infers a valid direction). Read it as a literal;
        // a non-literal step is undecidable → skip (conservative).
        let Some(step_node) = step_n else {
            continue;
        };
        let Some(step) = literal_number(step_node) else {
            continue; // computed step — cannot prove anything
        };

        // 1. step is 0 / NaN / ±Infinity. NaN/±Infinity can't be written as a bare
        //    numeric literal (the lexer only produces finite numbers), so in
        //    practice only a literal `0`/`0.0` reaches here — but the `is_finite`
        //    guard is kept so the rule matches the runtime panic predicate exactly.
        if step == 0.0 || !step.is_finite() {
            out.push(AsDiagnostic {
                range: code_range(node),
                severity: Severity::Warning,
                code: "range-step".to_string(),
                message: "step must be a finite, non-zero number".to_string(),
                fix: None,
            });
            continue; // a zero/non-finite step subsumes any direction check
        }

        // 2. Direction mismatch — only decidable when start AND end are also
        //    numeric literals. Mirrors `resolve_step`: `start != end` and
        //    `sign(step) != sign(end − start)`.
        if let (Some(start), Some(end)) = (literal_number(start_n), literal_number(end_n)) {
            if start != end && (step > 0.0) != (end > start) {
                out.push(AsDiagnostic {
                    range: code_range(node),
                    severity: Severity::Warning,
                    code: "range-step".to_string(),
                    message: format!(
                        "step {} moves away from end ({}); range can never progress",
                        format_number(step),
                        format_number(end),
                    ),
                    fix: None,
                });
                continue; // a mismatch subsumes the float advisory
            }
        }

        // 3. Advisory: a float (non-integer) step inside a match PATTERN. The
        //    stride membership test is exact-equality on floats and is therefore
        //    fragile (§3.8). Loops/value ranges accumulate the same rounding but
        //    are not predicates, so they are NOT flagged.
        if is_pattern && step.fract() != 0.0 {
            out.push(AsDiagnostic {
                range: code_range(node),
                severity: Severity::Warning,
                code: "range-step".to_string(),
                message: "float step in a range pattern may not match exactly; consider a guard"
                    .to_string(),
                fix: None,
            });
        }
    }
    out
}

/// Read `node` as a numeric literal value, transparently through a leading unary
/// `-`/`+` and parentheses (so `-2`, `(0)`, `-(0.25)` are all literals). Returns
/// `None` for anything non-literal (a name, call, computed expression), keeping
/// the rule conservative.
fn literal_number(node: &ResolvedNode) -> Option<f64> {
    use SyntaxKind::*;
    match node.kind() {
        Literal => {
            let tok = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| !t.kind().is_trivia())?;
            if tok.kind() == Number {
                parse_number_text(tok.text())
            } else {
                None // a string/bool/nil literal is not a number
            }
        }
        ParenExpr => {
            let inner = node.children().find(|c| is_expr_kind(c.kind()))?;
            literal_number(inner)
        }
        UnaryExpr => {
            let op = node
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| matches!(t.kind(), Minus | Plus))?
                .kind();
            let operand = node.children().find(|c| is_expr_kind(c.kind()))?;
            let v = literal_number(operand)?;
            Some(if op == Minus { -v } else { v })
        }
        _ => None,
    }
}

/// The CST expression kinds that can appear as a range operand (start/end/step).
/// Mirrors `is_expr_kind` in `src/compile/mod.rs` for the cases we recurse into.
fn is_expr_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal
            | NameRef
            | UnaryExpr
            | BinaryExpr
            | ParenExpr
            | CallExpr
            | MemberExpr
            | IndexExpr
            | ArrowExpr
            | AssignExpr
            | ArrayExpr
            | ObjectExpr
            | TemplateExpr
            | OptMemberExpr
            | TryExpr
            | UnwrapExpr
            | TernaryExpr
            | AwaitExpr
            | YieldExpr
            | MatchExpr
            | RangeExpr
    )
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    fn count(src: &str, code: &str) -> usize {
        analyze(src)
            .diagnostics
            .iter()
            .filter(|d| d.code == code)
            .count()
    }
    fn has(src: &str, code: &str) -> bool {
        count(src, code) > 0
    }

    #[test]
    fn zero_step_in_for_range_flagged() {
        let src = "for (i in 1..10 step 0){}\n";
        assert_eq!(
            count(src, "range-step"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn ascending_bounds_negative_step_mismatch() {
        let src = "for (i in 1..10 step -2){}\n";
        assert_eq!(
            count(src, "range-step"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn descending_bounds_positive_step_mismatch() {
        let src = "for (i in 10..1 step 2){}\n";
        assert_eq!(
            count(src, "range-step"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn valid_stepped_range_not_flagged() {
        let src = "for (i in 1..10 step 2){}\n";
        assert!(!has(src, "range-step"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn nonliteral_step_is_silent() {
        let src = "let k = 2\nlet xs = 1..10 step k\n";
        assert!(!has(src, "range-step"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn float_step_in_pattern_flagged() {
        let src = "let n = 0\nmatch n { 0..=1 step 0.25 => 1, _ => 0 }\n";
        assert_eq!(
            count(src, "range-step"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn float_step_in_for_range_not_flagged() {
        let src = "for (i in 0..=1 step 0.25){}\n";
        assert!(!has(src, "range-step"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn mismatch_message_matches_runtime() {
        // `step -2 moves away from end (10)` — byte-identical to the runtime panic.
        let src = "for (i in 1..10 step -2){}\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "range-step")
            .unwrap();
        assert_eq!(
            d.message,
            "step -2 moves away from end (10); range can never progress"
        );
    }

    #[test]
    fn zero_step_message_matches_runtime() {
        let src = "for (i in 1..10 step 0){}\n";
        let d = analyze(src)
            .diagnostics
            .into_iter()
            .find(|d| d.code == "range-step")
            .unwrap();
        assert_eq!(d.message, "step must be a finite, non-zero number");
    }

    #[test]
    fn equal_bounds_never_mismatch() {
        // start == end is always valid (no direction to disagree with).
        let src = "let xs = 5..5 step -2\n";
        assert!(!has(src, "range-step"), "{:?}", analyze(src).diagnostics);
    }

    #[test]
    fn integer_step_in_pattern_not_flagged_as_float() {
        let src = "let n = 0\nmatch n { 0..=10 step 2 => 1, _ => 0 }\n";
        assert!(!has(src, "range-step"), "{:?}", analyze(src).diagnostics);
    }
}
