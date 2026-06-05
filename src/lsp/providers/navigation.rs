//! `textDocument/definition` over the resolver: a name use → its binding's decl.
//!
//! Single-file only: resolves the `NameRef` under the cursor through the cached
//! resolver result (`model.resolved.uses`) to a `Local`/`Upvalue` binding's
//! declaration range. Globals/unresolved return `None`, so the server falls back
//! to the workspace index (which owns cross-file + module-global navigation).

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::Resolution;
use tower_lsp::lsp_types::Range;

/// In-file definition: the decl range of the binding the `NameRef` at `offset`
/// resolves to (`Local`/`Upvalue`). Returns `None` for globals/unresolved (the
/// server then asks the workspace index, which handles cross-file + module
/// globals) or when no name sits under the cursor.
pub fn definition_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    // Find the NameRef whose byte range contains the offset (the innermost — a
    // NameRef has no NameRef descendants, so the first match is precise).
    let nameref = name_ref_at(model, offset)?;
    let res = model.resolved.uses.get(&nameref.text_range())?;
    let decl_range = match res {
        Resolution::Local(_) | Resolution::Upvalue(_) => {
            // The resolver keys uses by slot, not by decl_range; match the binding
            // by name (single-file fallback — the workspace index handles the
            // cross-file / top-level cases that need precise shadow resolution).
            let name = crate::syntax::resolve::ident_text(nameref)?;
            model
                .resolved
                .bindings
                .iter()
                .find(|b| b.name == name)
                .map(|b| b.decl_range)?
        }
        Resolution::Global(_) | Resolution::Unresolved => return None,
    };
    // `decl_range` is the whole declaration statement (e.g. the full `let y = 1`).
    // Narrow it to the binding's NAME token so the jump lands on the identifier,
    // mirroring `workspace.rs::name_range_of`.
    let target = name_token_range(model, decl_range).unwrap_or(decl_range);
    Some(crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        ByteSpan::from(target),
    ))
}

/// Narrow a declaration's full `decl_range` to its NAME token: find the node at
/// `decl_range` and return its first `Ident` token range. `None` if no node
/// matches exactly or it has no `Ident` (the caller then uses the full range).
fn name_token_range(
    model: &SemanticModel,
    decl_range: cstree::text::TextRange,
) -> Option<cstree::text::TextRange> {
    let node = model
        .tree
        .descendants()
        .find(|n| n.text_range() == decl_range)?;
    node.children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text_range())
}

/// The `NameRef` node whose half-open byte range `[start, end)` contains `offset`.
fn name_ref_at(model: &SemanticModel, offset: usize) -> Option<&ResolvedNode> {
    model.tree.descendants().find(|n| {
        n.kind() == SyntaxKind::NameRef && {
            let r = n.text_range();
            let start: usize = r.start().into();
            let end: usize = r.end().into();
            offset >= start && offset < end
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn resolves_local_let() {
        let src = "fn f() {\n  let y = 1\n  return y\n}\n";
        let m = model(src);
        let use_off = src.rfind('y').unwrap();
        let r = definition_in_file(&m, use_off).expect("def");
        assert_eq!(r.start.line, 1); // the `let y` line
    }

    #[test]
    fn resolves_param_used_in_body() {
        let src = "fn f(n) {\n  return n + 1\n}\n";
        let m = model(src);
        let use_off = src.rfind('n').unwrap(); // the `n` in `n + 1`
        let r = definition_in_file(&m, use_off).expect("def for param n");
        assert_eq!(r.start.line, 0); // the param on the `fn f(n)` line
    }

    #[test]
    fn global_use_returns_none_for_index_fallback() {
        // A top-level fn use resolves to a Global → in-file provider declines so the
        // server falls back to the workspace index.
        let src = "fn foo() { return 1 }\nlet x = foo()\n";
        let m = model(src);
        let use_off = src.rfind("foo").unwrap();
        assert!(definition_in_file(&m, use_off).is_none());
    }

    #[test]
    fn non_name_offset_is_none() {
        let src = "let x = 1\n";
        let m = model(src);
        let off = src.find('=').unwrap();
        assert!(definition_in_file(&m, off).is_none());
    }
}
