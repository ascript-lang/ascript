//! Comment attachment pass — classifies every comment token as either
//! LEADING the next statement/member (own line) or TRAILING the preceding one
//! (same line), and records blank-line information for leading comments.

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
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

// ---------------------------------------------------------------------------
// Attachment logic
// ---------------------------------------------------------------------------

/// Node kinds to which comments attach directly.
fn is_attachable(kind: SyntaxKind) -> bool {
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
            | FieldDecl
            | MethodDecl
            | EnumVariant
    )
}

/// Walk up to the nearest attachable ancestor (including `node` itself).
fn attachable_of(node: &ResolvedNode) -> Option<ResolvedNode> {
    let mut cur: Option<ResolvedNode> = Some(node.clone());
    while let Some(n) = cur {
        if is_attachable(n.kind()) {
            return Some(n);
        }
        cur = n.parent().cloned();
    }
    None
}

/// Build the comment attachment map for `root`.
pub fn attach(root: &ResolvedNode) -> CommentMap {
    use SyntaxKind::*;

    let mut map = CommentMap::default();

    // Comments waiting to be attached to the next non-trivia token's node.
    let mut pending: Vec<Leading> = Vec::new();
    // How many newline tokens have elapsed since the last non-trivia token.
    let mut newlines_since_content: usize = 0;
    // The attachable ancestor node of the most recently seen non-trivia token.
    let mut last_attachable: Option<ResolvedNode> = None;

    for el in root.descendants_with_tokens() {
        // `el` is a `ResolvedElementRef` — a `NodeOrToken<&ResolvedNode, &ResolvedToken>`.
        // `into_token()` consumes the element ref and returns `Option<&ResolvedToken>`.
        let tok = match el.into_token() {
            Some(t) => t,
            None => continue,
        };

        match tok.kind() {
            Newline => {
                newlines_since_content += 1;
            }
            Whitespace => {
                // horizontal whitespace — does not affect line tracking
            }
            LineComment | BlockComment => {
                let text = tok.text().to_string();
                if newlines_since_content == 0 {
                    // Same line as the preceding content → trailing comment on that node.
                    if let Some(ref a) = last_attachable {
                        // Only attach the first trailing comment (subsequent ones are rare;
                        // this simple policy matches what the printer needs).
                        map.trailing.entry(a.text_range()).or_insert(text);
                    } else {
                        // Comment before any content: treat as pending leading.
                        pending.push(Leading { text, blank_before: false });
                    }
                } else {
                    // Own line → leading comment for the next statement.
                    let blank_before = newlines_since_content >= 2;
                    pending.push(Leading { text, blank_before });
                }
                // A comment itself does not reset `newlines_since_content` —
                // only non-trivia tokens do.  But we *do* count newlines
                // between the comment and the next token normally.
                // (newlines_since_content is NOT reset here.)
            }
            _ => {
                // Non-trivia token: flush pending leading comments onto this
                // token's nearest attachable ancestor.
                if !pending.is_empty() {
                    // `tok.parent()` returns `&ResolvedNode` (never None for tokens).
                    let parent: &ResolvedNode = tok.parent();
                    if let Some(a) = attachable_of(parent) {
                        map.leading
                            .entry(a.text_range())
                            .or_default()
                            .append(&mut pending);
                    } else {
                        pending.clear();
                    }
                }
                // Record which attachable node this content token belongs to.
                last_attachable = attachable_of(tok.parent());
                newlines_since_content = 0;
            }
        }
    }

    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn first_stmt(root: &ResolvedNode) -> ResolvedNode {
        root.children().next().unwrap().clone()
    }

    #[test]
    fn standalone_comment_is_leading() {
        let root = parse_to_tree("// hello\nx\n");
        let map = attach(&root);
        let s = first_stmt(&root); // the ExprStmt for `x`
        let lead = map.leading.get(&s.text_range()).expect("leading on first stmt");
        assert_eq!(lead.len(), 1);
        assert_eq!(lead[0].text, "// hello");
    }

    #[test]
    fn same_line_comment_is_trailing() {
        let root = parse_to_tree("x // tail\ny\n");
        let map = attach(&root);
        let s = first_stmt(&root); // ExprStmt for `x`
        assert_eq!(map.trailing.get(&s.text_range()).map(|t| t.as_str()), Some("// tail"));
    }

    #[test]
    fn blank_line_before_comment_is_recorded() {
        let root = parse_to_tree("a\n\n// grouped\nb\n");
        let map = attach(&root);
        let b = root.children().nth(1).unwrap();
        let lead = map.leading.get(&b.text_range()).expect("leading on b");
        assert!(lead[0].blank_before, "2+ newlines before comment → blank line preserved");
    }
}
