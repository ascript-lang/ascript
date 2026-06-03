//! Hand-written support layer for the generated typed AST.
//!
//! The generated node accessors (in `ast_nodes.rs`, emitted by `build.rs`) call
//! these generic helpers to read structured children off a `ResolvedNode`. The
//! `AstNode` trait is the contract every generated struct/enum implements so the
//! helpers can be generic over node type.

use crate::syntax::cst::{ResolvedNode, ResolvedToken};
use crate::syntax::kind::SyntaxKind;

/// A typed wrapper over a CST node. Implemented (via codegen) by every concrete
/// node struct and every alternation enum.
pub trait AstNode: Sized {
    /// Try to view `node` as `Self`. Returns `None` if the node's kind doesn't match.
    fn cast(node: ResolvedNode) -> Option<Self>;
    /// The underlying CST node.
    fn syntax(&self) -> &ResolvedNode;
}

/// First direct child node castable to `T`.
pub(crate) fn child<T: AstNode>(n: &ResolvedNode) -> Option<T> {
    n.children().find_map(|c| T::cast(c.clone()))
}

/// The `i`-th direct child node castable to `T` (0-based).
pub(crate) fn nth_child<T: AstNode>(n: &ResolvedNode, i: usize) -> Option<T> {
    n.children().filter_map(|c| T::cast(c.clone())).nth(i)
}

/// All direct child nodes castable to `T`, in source order.
pub(crate) fn children<T: AstNode>(n: &ResolvedNode) -> impl Iterator<Item = T> + '_ {
    n.children().filter_map(|c| T::cast(c.clone()))
}

/// First direct child token of the given kind.
pub(crate) fn token(n: &ResolvedNode, kind: SyntaxKind) -> Option<ResolvedToken> {
    n.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == kind)
        .cloned()
}

/// First direct child token whose kind is one of `kinds`.
pub(crate) fn token_in(n: &ResolvedNode, kinds: &[SyntaxKind]) -> Option<ResolvedToken> {
    n.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| kinds.contains(&t.kind()))
        .cloned()
}
