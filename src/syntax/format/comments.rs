//! Comment attachment pass — full implementation in Task 2.

use crate::syntax::cst::ResolvedNode;
use cstree::text::TextRange;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct CommentMap {
    pub leading: HashMap<TextRange, Vec<Leading>>,
    pub trailing: HashMap<TextRange, String>,
}

#[derive(Debug, Clone)]
pub struct Leading {
    pub text: String,
    pub blank_before: bool,
}

/// Build the comment map for `root`. (Task 1 stub: returns empty.)
pub fn attach(_root: &ResolvedNode) -> CommentMap {
    CommentMap::default()
}
