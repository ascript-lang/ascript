//! `textDocument/rename` family. `linked_editing_ranges` (Phase 4) returns the
//! same-file occurrences of the LOCAL identifier under the cursor so the editor
//! renames them live as one types. Tag-pairs are a documented future hook (HTML
//! templates) and are NOT implemented (spec §2 non-goals).
//!
//! Occurrences are matched by BINDING IDENTITY (reusing `navigation::BindingId`
//! and the tight-`Ident` occurrence logic from `highlight.rs`), so two sibling
//! same-name locals never cross-contaminate and a global is refused (an unsafe
//! cross-file/late-bound target for live linked editing).

use crate::lsp::providers::navigation::{self, BindingId};
use crate::lsp::providers::token_spans::token_at;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use tower_lsp::lsp_types::Range;

use crate::lsp::model::SemanticModel;

/// The same-file ranges (decl + every use) of the LOCAL/UPVALUE binding the
/// identifier at byte `offset` resolves to. Returns `None` for a global, an
/// unresolved name, a member access, or when the cursor is not on an identifier —
/// those are not safe for live linked editing within one file.
pub fn linked_editing_ranges(model: &SemanticModel, offset: usize) -> Option<Vec<Range>> {
    // Must be on an identifier token.
    let tok = token_at(model, offset)?;
    if tok.kind != SyntaxKind::Ident {
        return None;
    }
    let name = tok.text.clone();

    // The binding IDENTITY the cursor resolves to (a use site → frame-aware; a decl
    // name token → the binding whose decl_range contains the cursor).
    let id = navigation::binding_id_for(model, offset)
        .or_else(|| binding_id_at_decl_site(model, offset, &name))?;

    // Refuse globals: only frame-local/upvalue bindings are linked-edit safe.
    let decl_range = match id {
        BindingId::Local(r) => r,
        BindingId::Global(_) => return None,
    };

    let mut spans: Vec<TextRange> = Vec::new();

    // The declaration's own NAME token.
    if let Some(span) = name_token_range_in(model, decl_range, &name) {
        spans.push(span);
    }

    // Every `NameRef` use that resolves to the SAME binding → its tight Ident span.
    for nameref in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
    {
        if navigation::binding_id_for_node(model, nameref).as_ref() != Some(&id) {
            continue;
        }
        if let Some(span) = ident_token_range(nameref) {
            spans.push(span);
        }
    }

    if spans.is_empty() {
        return None;
    }

    // Dedup + order by start offset.
    let mut keyed: Vec<(usize, usize)> = spans
        .iter()
        .map(|s| (usize::from(s.start()), usize::from(s.end())))
        .collect();
    keyed.sort_unstable();
    keyed.dedup();

    Some(
        keyed
            .into_iter()
            .map(|(s, e)| {
                crate::lsp::convert::byte_span_to_range(
                    &model.text,
                    &model.line_index,
                    crate::check::ByteSpan { start: s, end: e },
                )
            })
            .collect(),
    )
}

/// When the cursor sits directly ON a declaration's name token (not a use), the
/// binding IDENTITY of the binding whose `decl_range` contains `offset` and whose
/// name is `name`. Picks the INNERMOST (smallest) such range.
fn binding_id_at_decl_site(model: &SemanticModel, offset: usize, name: &str) -> Option<BindingId> {
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

/// The first `Ident` TOKEN's range within `node` (its tight span).
fn ident_token_range(node: &ResolvedNode) -> Option<TextRange> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text_range())
}

/// Narrow a declaration's full `decl_range` to the matching NAME token.
fn name_token_range_in(
    model: &SemanticModel,
    decl_range: TextRange,
    name: &str,
) -> Option<TextRange> {
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

#[cfg(test)]
mod linked_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn links_local_let_occurrences() {
        let src = "fn f() {\n  let y = 1\n  return y + y\n}\n";
        let m = model(src);
        let off = src.find("let y").unwrap() + 4; // on the `y` of `let y`
        let ranges = linked_editing_ranges(&m, off).expect("linked ranges");
        // decl + two uses = 3 occurrences.
        assert_eq!(ranges.len(), 3, "{ranges:?}");
    }

    #[test]
    fn refuses_global_identifier() {
        let src = "fn top() {}\ntop()\n";
        let m = model(src);
        let off = src.rfind("top").unwrap();
        assert!(linked_editing_ranges(&m, off).is_none());
    }
}
