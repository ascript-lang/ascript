//! Typed AST: thin wrappers over the CST, generated from `ascript.ungram`.
#[allow(dead_code)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/ast_nodes.rs"));
}
pub use generated::*;

#[cfg(test)]
mod tests {
    use crate::syntax::parse_to_tree;
    use crate::syntax::kind::SyntaxKind;

    #[test]
    fn cast_source_file_then_find_let() {
        let root = parse_to_tree("let x = 1");
        let file = super::SourceFile::cast(root).expect("root is SourceFile");
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
        assert!(matches!(super::Expr::cast(bin), Some(super::Expr::BinaryExpr(_))));
    }
}
