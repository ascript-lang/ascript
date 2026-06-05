//! `textDocument/documentHighlight`: read/write occurrences of the identifier
//! under the cursor. The decl + every resolved use of the same binding, each
//! tagged Read or Write (an assignment target is a Write).

use crate::lsp::model::SemanticModel;
use crate::lsp::providers::token_spans::token_at;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use tower_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind};

/// Highlights for the identifier at byte `offset`. Returns `None` when the cursor
/// is not on an identifier or it resolves to nothing in-file.
///
/// A `NameRef` node's `text_range()` carries leading trivia (the resolver keys
/// `uses` by that padded range), so this walks the CST `NameRef`/decl nodes and
/// takes the TIGHT `Ident`-token span for both the highlight ranges and the
/// read/write classification — never the padded `uses` key.
pub fn document_highlights(model: &SemanticModel, offset: usize) -> Option<Vec<DocumentHighlight>> {
    let tok = token_at(model, offset)?;
    if tok.kind != SyntaxKind::Ident {
        return None;
    }
    let name = tok.text.clone();

    let mut out: Vec<DocumentHighlight> = Vec::new();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    // The declaration name span (Write — the binding/decl site).
    if let Some(b) = model.resolved.bindings.iter().find(|b| b.name == name) {
        if let Some(span) = name_token_range_in(model, b.decl_range, &name) {
            let (s, e) = (usize::from(span.start()), usize::from(span.end()));
            if seen.insert((s, e)) {
                out.push(highlight(model, s, e, DocumentHighlightKind::WRITE));
            }
        }
    }

    // The set of write-target Ident byte spans: the LHS `NameRef`'s tight Ident
    // token of every `AssignExpr`.
    let write_spans = assignment_target_spans(model);

    // Every `NameRef` use of this name → its tight Ident span, Read or Write.
    for nameref in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
    {
        let Some(span) = ident_token_range(nameref) else {
            continue;
        };
        let (s, e) = (usize::from(span.start()), usize::from(span.end()));
        if model.text[s..e] != *name {
            continue;
        }
        let kind = if write_spans.contains(&(s, e)) {
            DocumentHighlightKind::WRITE
        } else {
            DocumentHighlightKind::READ
        };
        if seen.insert((s, e)) {
            out.push(highlight(model, s, e, kind));
        }
    }

    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Tight Ident-token byte spans of every assignment-TARGET `NameRef` (the first
/// `NameRef` child of an `AssignExpr`).
fn assignment_target_spans(model: &SemanticModel) -> std::collections::HashSet<(usize, usize)> {
    let mut set = std::collections::HashSet::new();
    for assign in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::AssignExpr)
    {
        if let Some(target) = assign.children().find(|c| c.kind() == SyntaxKind::NameRef) {
            if let Some(span) = ident_token_range(target) {
                set.insert((usize::from(span.start()), usize::from(span.end())));
            }
        }
    }
    set
}

/// The first `Ident` TOKEN's range within `node` (its tight span, excluding the
/// node's leading trivia).
fn ident_token_range(node: &ResolvedNode) -> Option<TextRange> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text_range())
}

/// Narrow a declaration's full `decl_range` to the matching NAME token: the node
/// at `decl_range`, its first `Ident` token whose text is `name` (else its first
/// `Ident`).
fn name_token_range_in(model: &SemanticModel, decl_range: TextRange, name: &str) -> Option<TextRange> {
    let node = model
        .tree
        .descendants()
        .find(|n| n.text_range() == decl_range)?;
    let idents: Vec<_> = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| (t.text().to_string(), t.text_range()))
        .collect();
    idents
        .iter()
        .find(|(t, _)| t == name)
        .or_else(|| idents.first())
        .map(|(_, r)| *r)
}

fn highlight(
    model: &SemanticModel,
    start: usize,
    end: usize,
    kind: DocumentHighlightKind,
) -> DocumentHighlight {
    DocumentHighlight {
        range: crate::lsp::convert::byte_span_to_range(
            &model.text,
            &model.line_index,
            crate::check::ByteSpan { start, end },
        ),
        kind: Some(kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn highlights_read_and_write_occurrences() {
        let src = "let count = 0\ncount = count + 1\n";
        let m = model(src);
        // Cursor on the `count` in the assignment target (line 1, char 0).
        let off = src.find("count = count").unwrap();
        let hs = document_highlights(&m, off).expect("highlights");
        // At least: decl (write) + LHS (write) + RHS (read).
        let writes = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::WRITE))
            .count();
        let reads = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::READ))
            .count();
        assert!(writes >= 1, "{hs:?}");
        assert!(reads >= 1, "{hs:?}");
    }

    #[test]
    fn none_off_an_identifier() {
        let m = model("let x = 1\n");
        // byte 6 is the `=` operator region (after "let x ").
        assert!(document_highlights(&m, 6).is_none());
    }
}
