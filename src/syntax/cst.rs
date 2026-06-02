//! cstree integration: type aliases for our concrete syntax tree and helpers to
//! build it. `SyntaxKind` is used directly as cstree's syntax type (no separate
//! Language marker is needed in cstree 0.14).

use crate::syntax::kind::SyntaxKind;

/// Red-tree node handle, parameterized by our syntax kind.
pub type SyntaxNode = cstree::syntax::SyntaxNode<SyntaxKind>;
/// Red-tree token handle, parameterized by our syntax kind.
pub type SyntaxToken = cstree::syntax::SyntaxToken<SyntaxKind>;
/// A `SyntaxNode` that has a resolver attached so token text can be recovered.
pub type ResolvedNode = cstree::syntax::ResolvedNode<SyntaxKind>;

#[cfg(test)]
mod tests {
    use super::*;
    use cstree::build::GreenNodeBuilder;

    #[test]
    fn builds_and_reads_back_one_token() {
        let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
        builder.start_node(SyntaxKind::Root);
        builder.token(SyntaxKind::Number, "42");
        builder.finish_node();
        let (green, cache) = builder.finish();

        let resolver = cache.unwrap().into_interner().unwrap();
        let root: ResolvedNode = SyntaxNode::new_root_with_resolver(green, resolver);

        assert_eq!(root.text().to_string(), "42");
    }
}
