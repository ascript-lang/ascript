//! cstree integration: type aliases for our concrete syntax tree and helpers to
//! build it. `SyntaxKind` is used directly as cstree's syntax type (no separate
//! Language marker is needed in cstree 0.14).

use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::lex;
use cstree::build::GreenNodeBuilder;

/// Red-tree node handle, parameterized by our syntax kind.
pub type SyntaxNode = cstree::syntax::SyntaxNode<SyntaxKind>;
/// Red-tree token handle, parameterized by our syntax kind.
pub type SyntaxToken = cstree::syntax::SyntaxToken<SyntaxKind>;
/// A `SyntaxNode` that has a resolver attached so token text can be recovered.
pub type ResolvedNode = cstree::syntax::ResolvedNode<SyntaxKind>;
/// A `SyntaxToken` that has a resolver attached so `.text()` works.
pub type ResolvedToken = cstree::syntax::ResolvedToken<SyntaxKind>;

/// Build a flat CST: a single `Root` node containing every lexeme (including
/// trivia) as a token, in source order. Temporary scaffolding — the parser plan
/// replaces this with real node structure. Proves the cstree builder + lexer
/// produce a lossless tree. Returns a `ResolvedNode` so `.text()` works.
pub fn build_flat_tree(src: &str) -> ResolvedNode {
    let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind::Root);
    for t in lex(src) {
        builder.token(t.kind, &t.text);
    }
    builder.finish_node();
    let (green, cache) = builder.finish();
    let resolver = cache.unwrap().into_interner().unwrap();
    SyntaxNode::new_root_with_resolver(green, resolver)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn flat_tree_round_trips_source() {
        let src = "let x = 1 // c\nfoo(`t${x}`)\n";
        let node = crate::syntax::build_flat_tree(src);
        assert_eq!(node.text().to_string(), src);
    }
}
