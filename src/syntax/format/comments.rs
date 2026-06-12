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
            | InterfaceDecl
            | ImportStmt
            | ExportStmt
            | FieldDecl
            | MethodDecl
            | MethodReq
            | EnumVariant
            // DEFER: a `defer` statement is a first-class attachable statement node;
            // without this, a leading comment before `defer` is lost during formatting.
            | DeferStmt
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

/// The attachable NODE sibling immediately preceding `tok` (a separator like `,`),
/// if any. Used so a same-line trailing comment after a list item's separator
/// (`Red, // …`) attaches to the ITEM, not the enclosing container.
fn prev_attachable_sibling(tok: &crate::syntax::cst::ResolvedToken) -> Option<ResolvedNode> {
    let mut prev = tok.prev_sibling_or_token();
    while let Some(el) = prev {
        if let Some(node) = el.as_node() {
            if is_attachable(node.kind()) {
                return Some((*node).clone());
            }
            // A non-attachable node sibling (unlikely for a separator) → stop.
            return None;
        }
        // Skip trivia/whitespace tokens between the item and its separator.
        prev = el.prev_sibling_or_token();
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
                        pending.push(Leading {
                            text,
                            blank_before: false,
                        });
                    }
                } else {
                    // Own line → leading comment for the next statement.
                    let blank_before = newlines_since_content >= 2;
                    pending.push(Leading { text, blank_before });
                }
                // A comment occupies its line, so it acts as "content" for the
                // purpose of blank-line tracking: reset the newline counter so the
                // newline that *terminates this comment's line* is not double-counted
                // toward the NEXT comment's `blank_before`. Without this, a run of
                // consecutive `//` lines accrues one extra newline per comment and
                // the formatter wrongly inserts a blank between them. A genuine
                // author blank line (>= 2 newlines between two comments) is still
                // preserved because those extra newlines are counted *after* this
                // reset, before the next comment.
                newlines_since_content = 0;
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
                // Record which attachable node this content token belongs to. A
                // trailing SEPARATOR (`,`/`;`) is a sibling of the item it follows
                // (e.g. an enum-variant `,` is a child of `EnumDecl`, not the
                // `EnumVariant`), so naively its attachable ancestor is the CONTAINER,
                // and a same-line trailing comment after `Red,` would wrongly attach to
                // the enum (then print on the `}` line). Prefer the immediately-
                // preceding attachable SIBLING so the comment stays on the item's line.
                last_attachable = if matches!(tok.kind(), Comma | Semicolon) {
                    prev_attachable_sibling(tok).or_else(|| attachable_of(tok.parent()))
                } else {
                    attachable_of(tok.parent())
                };
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
        let lead = map
            .leading
            .get(&s.text_range())
            .expect("leading on first stmt");
        assert_eq!(lead.len(), 1);
        assert_eq!(lead[0].text, "// hello");
    }

    #[test]
    fn same_line_comment_is_trailing() {
        let root = parse_to_tree("x // tail\ny\n");
        let map = attach(&root);
        let s = first_stmt(&root); // ExprStmt for `x`
        assert_eq!(
            map.trailing.get(&s.text_range()).map(|t| t.as_str()),
            Some("// tail")
        );
    }

    #[test]
    fn trailing_comment_after_variant_separator_attaches_to_the_variant() {
        // Regression: a `// …` after `Red,` must attach to the `EnumVariant(Red)`,
        // not the enclosing `EnumDecl` (otherwise the printer drops it onto the `}`
        // line). The `,` is a child of `EnumDecl`, so the fix prefers the preceding
        // attachable SIBLING. Both unit and payload variants are covered.
        let root = parse_to_tree("enum E {\n  Red, // r\n  Pair(int, int), // p\n}\n");
        let map = attach(&root);
        let enum_decl = first_stmt(&root);
        let variants: Vec<ResolvedNode> = enum_decl
            .children()
            .filter(|c| c.kind() == SyntaxKind::EnumVariant)
            .cloned()
            .collect();
        assert_eq!(variants.len(), 2);
        assert_eq!(
            map.trailing.get(&variants[0].text_range()).map(|t| t.as_str()),
            Some("// r"),
            "comment after `Red,` attaches to the Red variant"
        );
        assert_eq!(
            map.trailing.get(&variants[1].text_range()).map(|t| t.as_str()),
            Some("// p"),
            "comment after `Pair(int, int),` attaches to the Pair variant"
        );
        // It must NOT have leaked onto the enum itself.
        assert!(
            !map.trailing.contains_key(&enum_decl.text_range()),
            "no trailing comment should attach to the EnumDecl container"
        );
    }

    #[test]
    fn blank_line_before_comment_is_recorded() {
        let root = parse_to_tree("a\n\n// grouped\nb\n");
        let map = attach(&root);
        let b = root.children().nth(1).unwrap();
        let lead = map.leading.get(&b.text_range()).expect("leading on b");
        assert!(
            lead[0].blank_before,
            "2+ newlines before comment → blank line preserved"
        );
    }

    #[test]
    fn consecutive_comments_have_no_blank_between() {
        // Regression: a run of `//` lines with no author blank between them must
        // NOT accrue a `blank_before` — the comment line's terminating newline was
        // being double-counted toward the next comment.
        let root = parse_to_tree("// a\n// b\n// c\nlet x = 1\n");
        let map = attach(&root);
        let stmt = first_stmt(&root); // the LetStmt
        let lead = map
            .leading
            .get(&stmt.text_range())
            .expect("3 leading comments on the let");
        assert_eq!(lead.len(), 3);
        for (i, c) in lead.iter().enumerate() {
            assert!(
                !c.blank_before,
                "comment {i} ({:?}) must have no blank_before in a consecutive run",
                c.text
            );
        }
    }

    #[test]
    fn genuine_blank_between_comments_is_preserved() {
        // An author blank line (>= 2 newlines) between two comments is still kept.
        let root = parse_to_tree("// a\n\n// c\nlet x = 1\n");
        let map = attach(&root);
        let stmt = first_stmt(&root);
        let lead = map.leading.get(&stmt.text_range()).expect("2 leading");
        assert_eq!(lead.len(), 2);
        assert!(!lead[0].blank_before, "first comment: no blank before");
        assert!(
            lead[1].blank_before,
            "second comment: author blank line preserved"
        );
    }
}
