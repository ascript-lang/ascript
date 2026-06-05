//! `textDocument/documentHighlight`: read/write occurrences of the identifier
//! under the cursor, resolved by BINDING IDENTITY (not name text). The cursor's
//! name is mapped to the precise binding it resolves to (via the frame-aware
//! resolution in `navigation`); the decl + every `NameRef` use that resolves to
//! that SAME binding is included, each tagged Read or Write (an assignment target
//! is a Write). So two sibling same-name locals never cross-contaminate.

use crate::lsp::model::SemanticModel;
use crate::lsp::providers::navigation::{self, BindingId};
use crate::lsp::providers::token_spans::token_at;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use tower_lsp::lsp_types::{DocumentHighlight, DocumentHighlightKind};

/// Highlights for the identifier at byte `offset`. Returns `None` when the cursor
/// is not on an identifier or it resolves to nothing in-file.
///
/// Occurrences are matched by BINDING IDENTITY: the cursor's name is resolved to a
/// binding (its `decl_range` is the identity); a `NameRef` use is included ONLY if
/// it resolves to the SAME binding. A `NameRef` node's `text_range()` carries
/// leading trivia (the resolver keys `uses` by that padded range), so the emitted
/// highlight ranges + the read/write classification use the TIGHT `Ident`-token
/// span — never the padded `uses` key.
pub fn document_highlights(model: &SemanticModel, offset: usize) -> Option<Vec<DocumentHighlight>> {
    let tok = token_at(model, offset)?;
    if tok.kind != SyntaxKind::Ident {
        return None;
    }
    let name = tok.text.clone();

    // The binding IDENTITY the cursor resolves to. The cursor may sit on a USE (a
    // `NameRef`, resolved frame-aware) OR directly on the DECL name token (no
    // `NameRef` there → fall back to the binding whose decl_range contains the cursor
    // and whose name matches).
    let id = navigation::binding_id_for(model, offset)
        .or_else(|| binding_id_at_decl_site(model, offset, &name))?;

    // The decl_range whose name token we emit as the Write decl site (the local's own
    // range, or — for a global — the matching module-global decl site).
    let decl_range = decl_range_for(model, &id, &name);

    let mut out: Vec<DocumentHighlight> = Vec::new();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    // The declaration's own name token (Write — the binding/decl site).
    if let Some(dr) = decl_range {
        if let Some(span) = name_token_range_in(model, dr, &name) {
            let (s, e) = (usize::from(span.start()), usize::from(span.end()));
            if seen.insert((s, e)) {
                out.push(highlight(model, s, e, DocumentHighlightKind::WRITE));
            }
        }
    }

    // The set of write-target Ident byte spans: the LHS `NameRef`'s tight Ident
    // token of every `AssignExpr`.
    let write_spans = assignment_target_spans(model);

    // Every `NameRef` use that resolves to the SAME binding → its tight Ident span,
    // Read or Write.
    for nameref in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
    {
        if navigation::binding_id_for_node(model, nameref).as_ref() != Some(&id) {
            continue;
        }
        let Some(span) = ident_token_range(nameref) else {
            continue;
        };
        let (s, e) = (usize::from(span.start()), usize::from(span.end()));
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

/// When the cursor sits directly ON a declaration's name token (not a use), the
/// binding IDENTITY of the binding whose `decl_range` contains `offset` and whose
/// name is `name`. Picks the INNERMOST (smallest) such decl_range so a name
/// re-declared in a nested scope resolves to the right binding. A module-global
/// binding maps to `BindingId::Global(name)`.
fn binding_id_at_decl_site(
    model: &SemanticModel,
    offset: usize,
    name: &str,
) -> Option<BindingId> {
    model
        .resolved
        .bindings
        .iter()
        .filter(|b| {
            b.name == name
                && offset >= usize::from(b.decl_range.start())
                && offset < usize::from(b.decl_range.end())
        })
        .min_by_key(|b| u32::from(b.decl_range.end()) - u32::from(b.decl_range.start()))
        .map(|b| {
            if b.is_global {
                BindingId::Global(b.name.clone())
            } else {
                BindingId::Local(b.decl_range)
            }
        })
}

/// The decl_range whose name token marks the Write decl site for `id`. A local is
/// its own `decl_range`; a global is the module-global binding decl with that name.
fn decl_range_for(model: &SemanticModel, id: &BindingId, name: &str) -> Option<TextRange> {
    match id {
        BindingId::Local(r) => Some(*r),
        BindingId::Global(_) => model
            .resolved
            .bindings
            .iter()
            .find(|b| b.is_global && b.name == name)
            .map(|b| b.decl_range),
    }
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

    #[test]
    fn highlights_resolve_by_binding_identity_not_name_text() {
        // Two sibling fns each with a `let x` + a `return x`. Highlighting a's `x`
        // must return EXACTLY a's decl + a's use (2 ranges) — NOT b's `x`.
        let src = "fn a() {\n  let x = 1\n  return x\n}\nfn b() {\n  let x = 2\n  return x\n}\n";
        let m = model(src);
        // Cursor on a's decl `x` (the `let x` on line 1).
        let decl_off = src.find("let x").unwrap() + "let ".len();
        let hs = document_highlights(&m, decl_off).expect("highlights");
        assert_eq!(hs.len(), 2, "a's x: decl + one use, NOT b's x — got {hs:?}");
        // Exactly one Write (the decl) and one Read (the `return x` in a).
        let writes = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::WRITE))
            .count();
        let reads = hs
            .iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::READ))
            .count();
        assert_eq!(writes, 1, "{hs:?}");
        assert_eq!(reads, 1, "{hs:?}");
        // None of the highlighted ranges may fall in b's body (after the `fn b`).
        let b_start = src.find("fn b").unwrap() as u32;
        for h in &hs {
            let off = crate::lsp::convert::char_to_byte(
                &m.text,
                m.line_index.offset(h.range.start),
            ) as u32;
            assert!(off < b_start, "highlight leaked into b's scope: {h:?}");
        }
    }
}
