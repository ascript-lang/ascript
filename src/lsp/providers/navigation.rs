//! `textDocument/definition` over the resolver: a name use → its binding's decl.
//!
//! Single-file only: resolves the `NameRef` under the cursor through the cached
//! resolver result (`model.resolved.uses`) to a `Local`/`Upvalue` binding's
//! declaration range. Globals/unresolved return `None`, so the server falls back
//! to the workspace index (which owns cross-file + module-global navigation).
//!
//! SCOPE-AWARE: a `Local(slot)`/`Upvalue(slot)` use is mapped to the EXACT binding
//! in the frame it belongs to (not the first binding by name), so two sibling
//! same-name locals and inner-block shadowing resolve correctly. The mapping reuses
//! the resolver's own structures: `model.resolved.frames` (keyed by the frame node's
//! `(SyntaxKind, TextRange)`) gives every frame's source range + upvalue chain;
//! `model.resolved.bindings` carries each binding's `slot` + `decl_range`. A
//! binding's OWNING frame is the innermost frame whose range contains its
//! `decl_range`; the use's owning frame is the innermost frame containing the use.

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Binding, Resolution, UpvalueDescriptor};
use cstree::text::TextRange;
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
    let use_range = nameref.text_range();
    let binding = match res {
        Resolution::Local(slot) => binding_for_local(model, use_range, *slot)?,
        Resolution::Upvalue(idx) => binding_for_upvalue(model, use_range, *idx)?,
        Resolution::Global(_) | Resolution::Unresolved => return None,
    };
    let decl_range = binding.decl_range;
    // `decl_range` is the whole declaration statement (e.g. the full `let y = 1`).
    // Narrow it to the binding's NAME token so the jump lands on the identifier,
    // mirroring `workspace.rs::name_range_of`.
    let target = name_token_range_for(model, decl_range, &binding.name).unwrap_or(decl_range);
    Some(crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        ByteSpan::from(target),
    ))
}

/// The binding a `Local(slot)` use resolves to: the binding with `slot` whose
/// OWNING frame is the innermost frame containing the use. (The resolver guarantees
/// a `Local` use sees a binding declared in its own frame.)
fn binding_for_local(model: &SemanticModel, use_range: TextRange, slot: u32) -> Option<&Binding> {
    let frame = innermost_frame_containing(model, use_range.start().into())?;
    binding_in_frame(model, frame, slot)
}

/// The binding an `Upvalue(idx)` use resolves to: follow the upvalue chain from the
/// use's innermost frame up to the `ParentLocal { slot }` that introduces it, then
/// pick the binding with that slot in that parent frame.
fn binding_for_upvalue(model: &SemanticModel, use_range: TextRange, idx: u32) -> Option<&Binding> {
    let mut frame = innermost_frame_containing(model, use_range.start().into())?;
    let mut idx = idx as usize;
    loop {
        let info = model.resolved.frames.get(&frame)?;
        match info.upvalues.get(idx)? {
            UpvalueDescriptor::ParentLocal { slot, .. } => {
                // The source binding lives in the immediate PARENT frame: the
                // innermost frame that strictly CONTAINS this frame's range.
                let parent = parent_frame(model, frame.1)?;
                return binding_in_frame(model, parent, *slot);
            }
            UpvalueDescriptor::ParentUpvalue(parent_idx) => {
                frame = parent_frame(model, frame.1)?;
                idx = *parent_idx as usize;
            }
        }
    }
}

/// The `(SyntaxKind, TextRange)` key of the INNERMOST frame whose range contains the
/// byte `offset` — the smallest such frame. `None` if no frame contains it (cannot
/// happen for a real use, which is always inside at least the `SourceFile` frame).
fn innermost_frame_containing(
    model: &SemanticModel,
    offset: usize,
) -> Option<(SyntaxKind, TextRange)> {
    model
        .resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: usize = r.start().into();
            let e: usize = r.end().into();
            offset >= s && offset < e
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

/// The INNERMOST frame that STRICTLY contains `child` (a smaller-or-equal range is
/// not a parent) — the parent frame of the frame whose range is `child`.
fn parent_frame(model: &SemanticModel, child: TextRange) -> Option<(SyntaxKind, TextRange)> {
    let cs: u32 = child.start().into();
    let ce: u32 = child.end().into();
    model
        .resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: u32 = r.start().into();
            let e: u32 = r.end().into();
            // Strictly contains `child` and is not `child` itself.
            s <= cs && e >= ce && (e - s) > (ce - cs)
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

/// The binding with `slot` whose OWNING frame is `frame`. A binding's owning frame is
/// the innermost frame whose range contains the binding's `decl_range`; we select the
/// binding with the matching slot whose owning frame equals `frame`.
fn binding_in_frame(
    model: &SemanticModel,
    frame: (SyntaxKind, TextRange),
    slot: u32,
) -> Option<&Binding> {
    model.resolved.bindings.iter().find(|b| {
        !b.is_global
            && b.slot == slot
            && owning_frame_of(model, b.decl_range).map(|f| f == frame).unwrap_or(false)
    })
}

/// The innermost frame whose range CONTAINS `decl_range` — the frame the binding
/// belongs to. (A binding's `decl_range` is nested inside its owning frame's range.)
fn owning_frame_of(model: &SemanticModel, decl_range: TextRange) -> Option<(SyntaxKind, TextRange)> {
    let ds: u32 = decl_range.start().into();
    let de: u32 = decl_range.end().into();
    model
        .resolved
        .frames
        .keys()
        .filter(|(_, r)| {
            let s: u32 = r.start().into();
            let e: u32 = r.end().into();
            s <= ds && e >= de
        })
        .min_by_key(|(_, r)| u32::from(r.end()) - u32::from(r.start()))
        .copied()
}

/// Narrow a declaration's full `decl_range` to the NAME token matching `name`: find
/// the node at `decl_range` and return its first `Ident` token whose text is `name`.
/// Falls back to the first `Ident` if none matches by text (e.g. the node IS the
/// ident). `None` if no node matches `decl_range` exactly.
fn name_token_range_for(
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
        .filter_map(|el| el.into_token().cloned())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .collect();
    idents
        .iter()
        .find(|t| t.text() == name)
        .or_else(|| idents.first())
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

    /// Line of the definition target for the use at byte `use_off`.
    fn def_line(m: &SemanticModel, use_off: usize) -> u32 {
        definition_in_file(m, use_off).expect("definition").start.line
    }

    #[test]
    fn resolves_local_let() {
        let src = "fn f() {\n  let y = 1\n  return y\n}\n";
        let m = model(src);
        let use_off = src.rfind('y').unwrap();
        assert_eq!(def_line(&m, use_off), 1); // the `let y` line
    }

    #[test]
    fn resolves_param_used_in_body() {
        let src = "fn f(n) {\n  return n + 1\n}\n";
        let m = model(src);
        let use_off = src.rfind('n').unwrap(); // the `n` in `n + 1`
        assert_eq!(def_line(&m, use_off), 0); // the param on the `fn f(n)` line
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

    // ── Scope-aware resolution (REQUIRED FIX 1) ──────────────────────────────────

    #[test]
    fn sibling_same_name_locals_resolve_to_own_binding() {
        // Two sibling functions each with a `let x`; the use in each must resolve to
        // ITS OWN `x`, not the first `x` by name.
        let src = "fn a() {\n  let x = 1\n  return x\n}\nfn b() {\n  let x = 2\n  return x\n}\n";
        let m = model(src);
        // First `return x` (in a) → the `let x` on line 1.
        let first_use = src.find("return x").unwrap() + "return ".len();
        assert_eq!(def_line(&m, first_use), 1, "use in a → a's x");
        // Second `return x` (in b) → the `let x` on line 5.
        let second_use = src.rfind("return x").unwrap() + "return ".len();
        assert_eq!(def_line(&m, second_use), 5, "use in b → b's x");
    }

    #[test]
    fn inner_block_let_shadows_outer() {
        // An inner-block `let x` shadows an outer `let x`; the use inside the block
        // must resolve to the INNER declaration, not the outer one.
        let src = "fn f() {\n  let x = 1\n  {\n    let x = 2\n    return x\n  }\n}\n";
        let m = model(src);
        let inner_use = src.rfind("return x").unwrap() + "return ".len();
        assert_eq!(def_line(&m, inner_use), 3, "inner use → inner `let x` (line 3)");
    }

    #[test]
    fn param_used_in_body_resolves_to_param() {
        // A param used in the body resolves to the PARAM declaration (the `fn` line),
        // even when a same-name local would exist elsewhere.
        let src = "fn g(v) {\n  return v * 2\n}\n";
        let m = model(src);
        let use_off = src.find("return v").unwrap() + "return ".len();
        assert_eq!(def_line(&m, use_off), 0, "v resolves to the param");
    }

    #[test]
    fn upvalue_use_resolves_to_outer_local() {
        // A param captured as an upvalue by an inner closure resolves to the OUTER
        // param declaration.
        let src = "fn outer(p) {\n  fn inner() {\n    return p\n  }\n  return inner\n}\n";
        let m = model(src);
        let use_off = src.find("return p").unwrap() + "return ".len();
        // The use of `p` resolves via Upvalue to the outer param on line 0.
        assert_eq!(def_line(&m, use_off), 0, "captured p → outer param");
    }
}
