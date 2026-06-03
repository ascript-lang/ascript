//! Typed AST: thin wrappers over the CST, generated from `ascript.ungram`.
pub mod support;
pub use support::AstNode;

#[cfg_attr(not(test), allow(dead_code))]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/ast_nodes.rs"));
}
pub use generated::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::cst::ResolvedNode;
    use crate::syntax::kind::SyntaxKind;
    use crate::syntax::parse_to_tree;

    /// Find the first descendant of `kind` and cast it to the typed node `T`.
    fn first<T: AstNode>(src: &str, kind: SyntaxKind) -> T {
        let root = parse_to_tree(src);
        let node: ResolvedNode = root
            .descendants()
            .find(|n| n.kind() == kind)
            .unwrap_or_else(|| panic!("no {kind:?} node in {src:?}"))
            .clone();
        T::cast(node).unwrap_or_else(|| panic!("cast to typed node failed for {kind:?}"))
    }

    #[test]
    fn cast_source_file_then_find_let() {
        let root = parse_to_tree("let x = 1");
        let file = SourceFile::cast(root).expect("root is SourceFile");
        let has_let = file.syntax().descendants().any(|n| n.kind() == SyntaxKind::LetStmt);
        assert!(has_let);
    }

    #[test]
    fn expr_enum_casts_a_binary() {
        let root = parse_to_tree("1 + 2");
        let bin = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BinaryExpr)
            .expect("has a BinaryExpr")
            .clone();
        assert!(matches!(Expr::cast(bin), Some(Expr::BinaryExpr(_))));
    }

    #[test]
    fn binary_expr_lhs_rhs_op() {
        let bin: BinaryExpr = first("1 + 2", SyntaxKind::BinaryExpr);
        assert!(matches!(bin.lhs(), Some(Expr::Literal(_))));
        assert!(matches!(bin.rhs(), Some(Expr::Literal(_))));
        assert_eq!(bin.op(), Some(SyntaxKind::Plus));
    }

    #[test]
    fn binary_expr_other_operators() {
        assert_eq!(first::<BinaryExpr>("a * b", SyntaxKind::BinaryExpr).op(), Some(SyntaxKind::Star));
        assert_eq!(first::<BinaryExpr>("a == b", SyntaxKind::BinaryExpr).op(), Some(SyntaxKind::EqEq));
        assert_eq!(first::<BinaryExpr>("a && b", SyntaxKind::BinaryExpr).op(), Some(SyntaxKind::AmpAmp));
    }

    #[test]
    fn let_stmt_name_and_initializer() {
        let stmt: LetStmt = first("let x = 1", SyntaxKind::LetStmt);
        assert_eq!(stmt.ident_token().expect("name").text(), "x");
        assert!(matches!(stmt.expr(), Some(Expr::Literal(_))));
        assert_eq!(stmt.op(), Some(SyntaxKind::LetKw));
    }

    #[test]
    fn let_const_keyword_and_no_initializer() {
        let stmt: LetStmt = first("const y", SyntaxKind::LetStmt);
        assert_eq!(stmt.op(), Some(SyntaxKind::ConstKw));
        assert!(stmt.expr().is_none());
    }

    #[test]
    fn call_expr_callee_and_args() {
        let call: CallExpr = first("f(1, 2)", SyntaxKind::CallExpr);
        assert!(matches!(call.expr(), Some(Expr::NameRef(_))));
        let args: Vec<Expr> = call.arg_list().expect("arg list").exprs().collect();
        assert_eq!(args.len(), 2);
        assert!(args.iter().all(|a| matches!(a, Expr::Literal(_))));
    }

    #[test]
    fn fn_decl_name_params_body() {
        let f: FnDecl = first("fn f() {}", SyntaxKind::FnDecl);
        assert_eq!(f.ident_token().expect("name").text(), "f");
        assert!(f.param_list().is_some());
        assert!(f.block().is_some());
    }

    #[test]
    fn fn_decl_params_iterate() {
        let f: FnDecl = first("fn g(a, b, c) {}", SyntaxKind::FnDecl);
        let n = f.param_list().expect("params").params().count();
        assert_eq!(n, 3);
    }

    #[test]
    fn literal_and_name_ref_tokens() {
        // NameRef carries an ident token.
        let nr: NameRef = first("foo", SyntaxKind::NameRef);
        assert_eq!(nr.ident_token().expect("ident").text(), "foo");
        // Literal node exists and is reachable as an Expr variant.
        let root = parse_to_tree("42");
        let lit = root.descendants().find(|n| n.kind() == SyntaxKind::Literal);
        assert!(lit.is_some());
    }

    #[test]
    fn unary_expr_operand_and_op() {
        let u: UnaryExpr = first("-x", SyntaxKind::UnaryExpr);
        assert_eq!(u.op(), Some(SyntaxKind::Minus));
        assert!(matches!(u.expr(), Some(Expr::NameRef(_))));
        assert_eq!(first::<UnaryExpr>("!ok", SyntaxKind::UnaryExpr).op(), Some(SyntaxKind::Bang));
    }

    #[test]
    fn paren_expr_inner() {
        let p: ParenExpr = first("(1)", SyntaxKind::ParenExpr);
        assert!(matches!(p.expr(), Some(Expr::Literal(_))));
    }

    #[test]
    fn expr_stmt_inner() {
        let s: ExprStmt = first("foo()", SyntaxKind::ExprStmt);
        assert!(s.expr().is_some());
    }

    #[test]
    fn block_collects_statements() {
        let b: Block = first("{ let a = 1; let b = 2 }", SyntaxKind::Block);
        assert_eq!(b.stmts().count(), 2);
    }

    #[test]
    fn if_stmt_cond_then_else() {
        let s: IfStmt = first("if x { 1 } else { 2 }", SyntaxKind::IfStmt);
        assert!(matches!(s.cond(), Some(Expr::NameRef(_))));
        assert!(s.then().is_some(), "then block");
        assert!(s.block().is_some(), "else block");
    }

    #[test]
    fn while_stmt_cond_body() {
        let s: WhileStmt = first("while x { y }", SyntaxKind::WhileStmt);
        assert!(matches!(s.cond(), Some(Expr::NameRef(_))));
        assert!(s.body().is_some());
    }

    #[test]
    fn return_stmt_value() {
        assert!(first::<ReturnStmt>("return 1", SyntaxKind::ReturnStmt).expr().is_some());
        assert!(first::<ReturnStmt>("return", SyntaxKind::ReturnStmt).expr().is_none());
    }

    #[test]
    fn arrow_expr_params_and_body() {
        let a: ArrowExpr = first("(x) => x", SyntaxKind::ArrowExpr);
        assert!(a.param_list().is_some());
        // Expression-bodied arrow: the body is an Expr (no Block).
        assert!(a.expr().is_some());
        assert!(a.block().is_none());
        // Block-bodied arrow.
        let b: ArrowExpr = first("() => { 1 }", SyntaxKind::ArrowExpr);
        assert!(b.block().is_some());
    }

    #[test]
    fn index_expr_base_and_index() {
        let ix: IndexExpr = first("a[b]", SyntaxKind::IndexExpr);
        assert!(matches!(ix.base(), Some(Expr::NameRef(_))));
        assert!(matches!(ix.index(), Some(Expr::NameRef(_))));
    }

    #[test]
    fn member_expr_object_and_name() {
        let m: MemberExpr = first("a.b", SyntaxKind::MemberExpr);
        assert!(matches!(m.expr(), Some(Expr::NameRef(_))));
        assert_eq!(m.ident_token().expect("member name").text(), "b");
    }

    #[test]
    fn assign_expr_target_and_value() {
        let a: AssignExpr = first("x = 1", SyntaxKind::AssignExpr);
        assert!(matches!(a.target(), Some(Expr::NameRef(_))));
        assert!(matches!(a.value(), Some(Expr::Literal(_))));
    }

    #[test]
    fn ternary_expr_three_branches() {
        let t: TernaryExpr = first("a ? b : c", SyntaxKind::TernaryExpr);
        assert!(matches!(t.cond(), Some(Expr::NameRef(_))));
        assert!(matches!(t.then(), Some(Expr::NameRef(_))));
        assert!(matches!(t.els(), Some(Expr::NameRef(_))));
    }

    #[test]
    fn arg_list_exprs() {
        let call: CallExpr = first("g()", SyntaxKind::CallExpr);
        assert_eq!(call.arg_list().expect("args").exprs().count(), 0);
    }

    #[test]
    fn ast_node_trait_object_safe_helpers() {
        // The AstNode impl on an enum delegates syntax() to the active variant.
        let root = parse_to_tree("1 + 2");
        let bin = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BinaryExpr)
            .unwrap()
            .clone();
        let e = Expr::cast(bin).expect("expr");
        assert_eq!(e.syntax().kind(), SyntaxKind::BinaryExpr);
    }
}
