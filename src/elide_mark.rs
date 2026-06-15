//! ELIDE §4.3 — tree-walker AST marking pass.
//!
//! Given an [`ElisionSet`] (from `check::infer::elision_proofs`), this module
//! walks a legacy AST (`Vec<Stmt>`) and applies three elision marks:
//!
//! - **Row 1 (calls):** sets `ExprKind::Call.elide_args = true` on every call
//!   node whose `(span.start, span.end)` CHAR extent is in `set.calls`.
//! - **Row 2 (lets):** strips `Stmt::Let.ty` to `None` on every annotated let
//!   whose INITIALIZER's `(span.start, span.end)` is in `set.lets`.
//! - **Row 3 (fn_rets):** strips `Stmt::Fn.ret` to `None` on every fn whose
//!   `name_span.(start, end)` is in `set.fn_rets`.
//!
//! The walk is **recursive** — it descends into nested fn bodies, class method
//! bodies, block statements, and every expression sub-tree. Class method bodies
//! are traversed for completeness (they are never row-1 eligible in v1, but the
//! walk must not panic on them).
//!
//! ## Span convention (the cross-front-end key discipline, spec §4.3)
//!
//! The legacy front-end stores CHAR offsets in every `Span` (CLAUDE.md: "the
//! legacy front-end is CHAR-correct"). The `ElisionSet` stores CHAR-offset keys
//! (the collector converts the CST's BYTE ranges once via `ByteToCharMap`). So
//! the lookup is simply:
//!
//! ```text
//! key = (span.start as u32, span.end as u32)
//! ```
//!
//! **Fail-safe:** every lookup is exact-match. A miss means the check is KEPT —
//! this is always sound. A wrong match is structurally impossible within a single
//! parse (two nodes of the same kind with the same `(start, end)` can only arise
//! if they are the same node). The gate is **count parity**: `marked == consumed
//! (VM compiler) == |ElisionSet|` per module. Any span mismatch surfaces as
//! `marks < |set|`, which the gate catches loudly.
//!
//! ## Module scoping
//!
//! This module is called once per module, with THAT module's `ElisionSet`. Per-
//! module scoping is automatic — keys from a different module's source never
//! match the current module's char offsets. Cross-module span collisions are
//! structurally impossible (§4.3).
//!
//! ## Feature independence
//!
//! This module is **CORE** (no feature gate). It must build under
//! `--no-default-features`.

use crate::ast::{ArrayElem, CallArg, Expr, ExprKind, MethodDecl, ObjEntry, Stmt};
use crate::check::infer::elide::ElisionSet;

// ---------------------------------------------------------------------------
// MarkCounts — returned by mark_program for the count-parity gate (§6.4)
// ---------------------------------------------------------------------------

/// Counts of marks applied by [`mark_program`], split by kind.
///
/// The count-parity gate asserts:
/// ```text
/// MarkCounts::total() == compile_consumed_count == ElisionSet::len()
/// ```
/// per module and both feature configs (ELIDE §6.4, Gate 15).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkCounts {
    /// Number of `ExprKind::Call` nodes with `elide_args` set to `true`.
    pub calls: usize,
    /// Number of `Stmt::Let` nodes with `ty` stripped to `None`.
    pub lets: usize,
    /// Number of `Stmt::Fn` nodes with `ret` stripped to `None`.
    pub fn_rets: usize,
}

impl MarkCounts {
    /// The total number of marks applied, matching `ElisionSet::len()`.
    pub fn total(&self) -> usize {
        self.calls + self.lets + self.fn_rets
    }

    /// Whether no marks were applied (the untyped-corpus expectation).
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Walk `stmts` and apply elision marks from `set`.
///
/// Returns the [`MarkCounts`] for the count-parity gate. The marks are applied
/// IN-PLACE (the `Vec<Stmt>` is mutated). Calling this with an empty or wrong
/// `set` is safe — marks are simply not applied.
pub fn mark_program(stmts: &mut [Stmt], set: &ElisionSet) -> MarkCounts {
    let mut counts = MarkCounts::default();
    mark_stmts(stmts, set, &mut counts);
    counts
}

// ---------------------------------------------------------------------------
// Recursive statement walker
// ---------------------------------------------------------------------------

fn mark_stmts(stmts: &mut [Stmt], set: &ElisionSet, counts: &mut MarkCounts) {
    for stmt in stmts.iter_mut() {
        mark_stmt(stmt, set, counts);
    }
}

fn mark_stmt(stmt: &mut Stmt, set: &ElisionSet, counts: &mut MarkCounts) {
    match stmt {
        // ── Row 2: annotated let — strip `ty` if initializer span is proven ──
        Stmt::Let { ty, value, .. } => {
            // Only record a let site when the initializer is present and
            // the annotation exists.  The key is the INITIALIZER's char span
            // (mirroring `emit_check_local`'s `node_code_span(init)` →
            // `set.lets` lookup in the compiler).
            if let (Some(_), Some(init_expr)) = (ty.as_ref(), value.as_ref()) {
                let key = (init_expr.span.start as u32, init_expr.span.end as u32);
                if set.lets.contains(&key) {
                    *ty = None;
                    counts.lets += 1;
                }
            }
            // Recurse into the initializer (it may contain nested proven calls).
            if let Some(init_expr) = value.as_mut() {
                mark_expr(init_expr, set, counts);
            }
        }

        // ── Row 3: fn declaration — strip `ret` if name-span is proven ──
        Stmt::Fn {
            name_span,
            ret,
            body,
            ..
        } => {
            // The key is the fn's NAME-token CHAR span (mirroring the
            // compiler's fn-proto builder and the pass's `push_fn` call).
            let key = (name_span.start as u32, name_span.end as u32);
            if set.fn_rets.contains(&key) {
                *ret = None;
                counts.fn_rets += 1;
            }
            // Always recurse into the body — it may contain proven calls/lets.
            mark_stmts(body, set, counts);
        }

        // ── Plain expression statements ──
        Stmt::Expr(expr) => {
            mark_expr(expr, set, counts);
        }

        // ── Block ──
        Stmt::Block(stmts) => {
            mark_stmts(stmts, set, counts);
        }

        // ── If / while / for ──
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            mark_expr(cond, set, counts);
            mark_stmts(then_branch, set, counts);
            if let Some(els) = else_branch.as_mut() {
                mark_stmts(els, set, counts);
            }
        }
        Stmt::While { cond, body } => {
            mark_expr(cond, set, counts);
            mark_stmts(body, set, counts);
        }
        Stmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            mark_expr(start, set, counts);
            mark_expr(end, set, counts);
            if let Some(s) = step.as_mut() {
                mark_expr(s, set, counts);
            }
            mark_stmts(body, set, counts);
        }
        Stmt::ForOf { iter, body, .. } => {
            mark_expr(iter, set, counts);
            mark_stmts(body, set, counts);
        }

        // ── Return ──
        Stmt::Return(Some(expr)) => {
            mark_expr(expr, set, counts);
        }

        // ── Class — traverse method bodies (not eligible in v1 but must not panic) ──
        Stmt::Class { methods, .. } => {
            for method in methods.iter_mut() {
                mark_method(method, set, counts);
            }
        }

        // ── Export wraps another statement ──
        Stmt::Export(inner) => {
            mark_stmt(inner, set, counts);
        }

        // ── Defer — the call inside ──
        Stmt::Defer { call, .. } => {
            mark_expr(call, set, counts);
        }

        // ── Everything else (Break, Continue, Return(None), Enum, Interface,
        //    Import, LetDestructure, LetDestructureObject) — no sub-exprs to
        //    recurse into that can hold proven call sites in v1. ──
        _ => {}
    }
}

fn mark_method(method: &mut MethodDecl, set: &ElisionSet, counts: &mut MarkCounts) {
    // Methods are not row-1/3 eligible in v1 (no proof source for method args
    // or method return contracts yet), but we recurse to mark proven calls
    // INSIDE the method body.
    mark_stmts(&mut method.body, set, counts);
}

// ---------------------------------------------------------------------------
// Recursive expression walker
// ---------------------------------------------------------------------------

fn mark_expr(expr: &mut Expr, set: &ElisionSet, counts: &mut MarkCounts) {
    match &mut expr.kind {
        // ── Row 1: call expression — set elide_args if span is proven ──
        ExprKind::Call {
            callee,
            args,
            elide_args,
        } => {
            // The key is the CALL EXPRESSION's CHAR span — `(expr.span.start,
            // expr.span.end)`. This mirrors the compiler's `node_code_span(call)`
            // which trims leading trivia and uses the same start-to-close-paren
            // range as the legacy parser builds.
            let key = (expr.span.start as u32, expr.span.end as u32);
            if set.calls.contains(&key) {
                *elide_args = true;
                counts.calls += 1;
            }
            // Always recurse: the callee and args may themselves contain proven calls.
            mark_expr(callee, set, counts);
            mark_call_args(args, set, counts);
        }

        // ── Unary / Binary ──
        ExprKind::Unary { expr: inner, .. } => {
            mark_expr(inner, set, counts);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            mark_expr(lhs, set, counts);
            mark_expr(rhs, set, counts);
        }

        // ── Arrow (anonymous function expressions) ──
        ExprKind::Arrow { body, .. } => {
            use crate::ast::ArrowBody;
            match body.as_mut() {
                ArrowBody::Expr(e) => mark_expr(e, set, counts),
                ArrowBody::Block(stmts) => mark_stmts(stmts, set, counts),
            }
        }

        // ── Array / Object / Map literals ──
        ExprKind::Array(elems) => {
            mark_array_elems(elems, set, counts);
        }
        ExprKind::Object(entries) => {
            mark_obj_entries(entries, set, counts);
        }
        ExprKind::Map(entries) => {
            for e in entries.iter_mut() {
                mark_expr(&mut e.key, set, counts);
                mark_expr(&mut e.value, set, counts);
            }
        }

        // ── Index / Member / OptMember ──
        ExprKind::Index { object, index } => {
            mark_expr(object, set, counts);
            mark_expr(index, set, counts);
        }
        ExprKind::Member { object, .. } | ExprKind::OptMember { object, .. } => {
            mark_expr(object, set, counts);
        }

        // ── Assign ──
        ExprKind::Assign { target, value } => {
            mark_expr(target, set, counts);
            mark_expr(value, set, counts);
        }

        // ── Try / Unwrap ──
        ExprKind::Try(inner) | ExprKind::Unwrap(inner) => {
            mark_expr(inner, set, counts);
        }

        // ── Ternary ──
        ExprKind::Ternary { cond, then, els } => {
            mark_expr(cond, set, counts);
            mark_expr(then, set, counts);
            mark_expr(els, set, counts);
        }

        // ── Template string ──
        ExprKind::Template { parts } => {
            use crate::ast::TemplatePart;
            for part in parts.iter_mut() {
                if let TemplatePart::Expr(e) = part {
                    mark_expr(e.as_mut(), set, counts);
                }
            }
        }

        // ── Match ──
        ExprKind::Match { subject, arms } => {
            mark_expr(subject, set, counts);
            for arm in arms.iter_mut() {
                if let Some(g) = arm.guard.as_mut() {
                    mark_expr(g, set, counts);
                }
                mark_expr(&mut arm.body, set, counts);
            }
        }



        // ── Yield ──
        ExprKind::Yield(Some(inner)) => {
            mark_expr(inner.as_mut(), set, counts);
        }

        // ── Await ──
        ExprKind::Await(inner) => {
            mark_expr(inner.as_mut(), set, counts);
        }

        // ── Paren ──
        ExprKind::Paren(inner) => {
            mark_expr(inner.as_mut(), set, counts);
        }

        // ── Range ──
        ExprKind::Range { start, end, step, .. } => {
            mark_expr(start.as_mut(), set, counts);
            mark_expr(end.as_mut(), set, counts);
            if let Some(s) = step.as_mut() {
                mark_expr(s.as_mut(), set, counts);
            }
        }

        // ── Literals and name references — leaf nodes, nothing to recurse ──
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil
        | ExprKind::Ident(_)
        | ExprKind::Yield(None) => {}
    }
}

fn mark_call_args(args: &mut [CallArg], set: &ElisionSet, counts: &mut MarkCounts) {
    for arg in args.iter_mut() {
        match arg {
            CallArg::Pos(e) | CallArg::Spread(e) => mark_expr(e, set, counts),
            CallArg::Named { value, .. } => mark_expr(value, set, counts),
        }
    }
}

fn mark_array_elems(elems: &mut [ArrayElem], set: &ElisionSet, counts: &mut MarkCounts) {
    for elem in elems.iter_mut() {
        match elem {
            ArrayElem::Item(e) | ArrayElem::Spread(e) => mark_expr(e, set, counts),
        }
    }
}

fn mark_obj_entries(entries: &mut [ObjEntry], set: &ElisionSet, counts: &mut MarkCounts) {
    for entry in entries.iter_mut() {
        match entry {
            ObjEntry::KV(_, e) | ObjEntry::Spread(e) => mark_expr(e, set, counts),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::infer::elision_proofs;

    fn parse(src: &str) -> Vec<Stmt> {
        let tokens = crate::lexer::lex(src).expect("lex ok");
        crate::parser::parse(&tokens).expect("parse ok")
    }

    // ── helpers to find marked nodes ─────────────────────────────────────────

    fn has_elided_call(stmts: &[Stmt]) -> bool {
        stmts.iter().any(stmt_has_elided_call)
    }

    fn stmt_has_elided_call(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Expr(e) => expr_has_elided_call(e),
            Stmt::Let { value: Some(e), .. } => expr_has_elided_call(e),
            Stmt::Fn { body, .. } => has_elided_call(body),
            Stmt::Block(stmts) => has_elided_call(stmts),
            Stmt::If { cond, then_branch, else_branch } => {
                expr_has_elided_call(cond)
                    || has_elided_call(then_branch)
                    || else_branch.as_deref().map(has_elided_call).unwrap_or(false)
            }
            Stmt::Return(Some(e)) => expr_has_elided_call(e),
            Stmt::Export(inner) => stmt_has_elided_call(inner),
            _ => false,
        }
    }

    fn expr_has_elided_call(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Call { elide_args, callee, args } => {
                *elide_args
                    || expr_has_elided_call(callee)
                    || args.iter().any(|a| {
                        match a {
                            CallArg::Pos(e) | CallArg::Spread(e) => expr_has_elided_call(e),
                            CallArg::Named { value, .. } => expr_has_elided_call(value),
                        }
                    })
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                expr_has_elided_call(lhs) || expr_has_elided_call(rhs)
            }
            ExprKind::Unary { expr: inner, .. } => expr_has_elided_call(inner),
            _ => false,
        }
    }

    fn has_stripped_let_ty(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| matches!(s, Stmt::Let { ty: None, .. }))
    }

    fn has_stripped_fn_ret(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| matches!(s, Stmt::Fn { ret: None, .. }))
    }

    // ── core marking tests ────────────────────────────────────────────────────

    #[test]
    fn marks_proven_call() {
        let src = "fn f(a: int, b: string) {} f(1, \"x\")\n";
        let set = elision_proofs(src);
        assert!(!set.calls.is_empty(), "should have a proven call");
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(counts.calls, set.calls.len(), "call mark count must equal set.calls.len()");
        assert!(has_elided_call(&stmts), "expected elide_args=true on the call");
    }

    #[test]
    fn marks_proven_let() {
        let src = "let x: int = 5\n";
        let set = elision_proofs(src);
        assert!(!set.lets.is_empty(), "should have a proven let site");
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(counts.lets, set.lets.len(), "let mark count must equal set.lets.len()");
        assert!(has_stripped_let_ty(&stmts), "expected Let.ty = None after marking");
    }

    #[test]
    fn marks_proven_fn_ret() {
        let src = "fn g(): int { return 1 }\n";
        let set = elision_proofs(src);
        assert!(!set.fn_rets.is_empty(), "should have a proven fn_ret");
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(counts.fn_rets, set.fn_rets.len(), "fn_ret mark count must equal set.fn_rets.len()");
        assert!(has_stripped_fn_ret(&stmts), "expected Fn.ret = None after marking");
    }

    #[test]
    fn perturbed_key_marks_nothing() {
        let src = "fn f(a: int) {} f(1)\n";
        let set = elision_proofs(src);
        assert!(!set.is_empty(), "should have proven sites");
        // Use an empty set — nothing should be marked.
        let empty_set = ElisionSet::default();
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &empty_set);
        assert_eq!(counts.total(), 0, "empty set must mark nothing");
        assert!(!has_elided_call(&stmts));
    }

    #[test]
    fn counts_returned_accurately() {
        let src = "fn g(): int { return 1 }\nfn f(p: int){}\nlet x: int = 5\nf(x)\ng()\n";
        let set = elision_proofs(src);
        let set_total = set.len();
        assert!(set_total > 0, "typed program must have proven sites");
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(
            counts.total(),
            set_total,
            "MarkCounts::total() must equal ElisionSet::len()"
        );
    }

    #[test]
    fn nested_fn_bodies_traversed() {
        // A fn body containing a proven call — the walk must descend.
        let src = "fn f(p: int){}\nfn outer() { f(5) }\n";
        let set = elision_proofs(src);
        assert!(!set.calls.is_empty(), "should prove the inner call");
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert!(counts.calls > 0, "nested call must be marked");
    }

    #[test]
    fn class_bodies_traversed_no_panic() {
        // Methods are not eligible in v1 but the walk must not panic.
        let src = "class C { fn method() { let x = 1 } }\n";
        let set = elision_proofs(src);
        let mut stmts = parse(src);
        // Must not panic — even with an empty set.
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(counts.total(), 0, "no eligible marks in method-only program");
    }

    #[test]
    fn unproven_call_stays_unmarked() {
        // Mutated binding → not anchored → call not proven.
        let src = "fn f(p: int){}\nlet x: int = 5\nx = \"s\"\nf(x)\n";
        let set = elision_proofs(src);
        assert!(
            set.calls.is_empty(),
            "mutated-binding call must NOT be proven; set.calls={:?}",
            set.calls
        );
        let mut stmts = parse(src);
        let counts = mark_program(&mut stmts, &set);
        assert_eq!(counts.calls, 0, "unproven call must stay unmarked");
        assert!(!has_elided_call(&stmts));
    }
}
