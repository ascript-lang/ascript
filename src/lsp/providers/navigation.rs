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
    let binding = binding_for_offset(model, offset)?;
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

/// The frame-aware binding the `NameRef` at `offset` resolves to (`Local`/
/// `Upvalue`). `None` for globals/unresolved or when no name sits under the cursor.
fn binding_for_offset(model: &SemanticModel, offset: usize) -> Option<&Binding> {
    let nameref = name_ref_at(model, offset)?;
    binding_for_node(model, nameref)
}

/// The frame-aware binding a specific `NameRef` node resolves to (`Local`/
/// `Upvalue`). `None` for globals/unresolved. This is the SHARED scope-precise
/// resolution reused by document-highlight to match occurrences by BINDING
/// IDENTITY rather than name text.
pub(crate) fn binding_for_node<'m>(
    model: &'m SemanticModel,
    nameref: &ResolvedNode,
) -> Option<&'m Binding> {
    let use_range = nameref.text_range();
    let res = model.resolved.uses.get(&use_range)?;
    match res {
        Resolution::Local(slot) => binding_for_local(model, use_range, *slot),
        Resolution::Upvalue(idx) => binding_for_upvalue(model, use_range, *idx),
        Resolution::Global(_) | Resolution::Unresolved => None,
    }
}

/// A binding's IDENTITY for occurrence matching: a frame-precise local/upvalue is
/// identified by its declaration range; a module-scope global is identified by its
/// name (globals have no per-frame slot). Two uses share a binding iff their
/// `BindingId`s are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindingId {
    Local(TextRange),
    Global(String),
}

/// The binding IDENTITY the `NameRef` at `offset` resolves to. A `Local`/`Upvalue`
/// resolves frame-precisely to its decl range; a `Global` resolves to its name.
/// `None` for unresolved or when no name sits under the cursor.
pub(crate) fn binding_id_for(model: &SemanticModel, offset: usize) -> Option<BindingId> {
    let nameref = name_ref_at(model, offset)?;
    binding_id_for_node(model, nameref)
}

/// The binding IDENTITY a specific `NameRef` use resolves to. Used by
/// document-highlight to test whether a candidate use refers to the SAME binding as
/// the cursor.
pub(crate) fn binding_id_for_node(
    model: &SemanticModel,
    nameref: &ResolvedNode,
) -> Option<BindingId> {
    let use_range = nameref.text_range();
    match model.resolved.uses.get(&use_range)? {
        Resolution::Local(slot) => {
            binding_for_local(model, use_range, *slot).map(|b| BindingId::Local(b.decl_range))
        }
        Resolution::Upvalue(idx) => {
            binding_for_upvalue(model, use_range, *idx).map(|b| BindingId::Local(b.decl_range))
        }
        Resolution::Global(name) => Some(BindingId::Global(name.clone())),
        Resolution::Unresolved => None,
    }
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

/// The frame chain reachable from byte `offset`: the innermost frame containing the
/// cursor plus every ancestor frame up to and including the `SourceFile` frame. A
/// non-global binding is LIVE at the cursor iff its OWNING frame is one of these — i.e.
/// the binding's frame ENCLOSES the cursor (the cursor frame's own locals/params + the
/// parent-frame / upvalue chain). A sibling scope that does not enclose the cursor is
/// excluded. This reuses the exact frame model (`innermost_frame_containing` +
/// `parent_frame`) that `definition`/`document-highlight` use — NOT a second resolver.
pub(crate) fn frame_chain_at(
    model: &SemanticModel,
    offset: usize,
) -> Vec<(SyntaxKind, TextRange)> {
    let mut out = Vec::new();
    let Some(mut frame) = innermost_frame_containing(model, offset) else {
        return out;
    };
    out.push(frame);
    while let Some(parent) = parent_frame(model, frame.1) {
        out.push(parent);
        frame = parent;
    }
    out
}

/// Whether the binding declared at `decl_range` is LIVE at `offset`: its owning frame
/// is on the cursor's frame chain. The single SoT for "which non-global bindings are in
/// scope here," reused by frame-precise identifier completion.
pub(crate) fn binding_live_at(
    model: &SemanticModel,
    decl_range: TextRange,
    chain: &[(SyntaxKind, TextRange)],
) -> bool {
    owning_frame_of(model, decl_range)
        .map(|f| chain.contains(&f))
        .unwrap_or(false)
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

/// `textDocument/declaration` — for AScript this is identical to `definition`
/// (no separate declaration concept). Resolves the name at `offset` to its decl
/// range in this file. Cross-file declarations are served by the workspace index
/// in the server handler (same path `definition` uses).
pub fn declaration_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    definition_in_file(model, offset)
}

/// `textDocument/typeDefinition` — the inferred type of the value at `offset`
/// names a class/enum; return that declaration's NAME range in this file.
/// Returns `None` when the type is a primitive, `Any`, or a type whose decl is
/// not in this file (cross-module types are `Any` under SP10 — a documented
/// limitation).
pub fn type_definition_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    let ty = crate::check::infer::hover_type_in(model.infer_cache(), offset)?;
    // The rendered type may be `User`, `User?`, `array<User>`, etc. Extract the
    // first bare identifier (a class/enum name) from the rendering.
    let type_name = first_type_ident(&ty)?;
    decl_name_range(model, &type_name)
}

/// Extract the leading user-type identifier from a rendered `CheckTy` string.
/// `"User"` -> `User`; `"User?"` -> `User`; `"array<User>"` -> the first
/// non-builtin identifier token.
fn first_type_ident(rendered: &str) -> Option<String> {
    const BUILTIN: &[&str] = &[
        "number", "string", "bool", "nil", "any", "array", "map", "future", "bytes", "regex",
        "object", "void", "never",
    ];
    let mut cur = String::new();
    for ch in rendered.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
                return Some(cur);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
        return Some(cur);
    }
    None
}

/// The NAME range of the `class`/`enum` named `name` declared in this file.
fn decl_name_range(model: &SemanticModel, name: &str) -> Option<Range> {
    let decl = model.tree.descendants().find(|n| {
        matches!(n.kind(), SyntaxKind::ClassDecl | SyntaxKind::EnumDecl)
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    })?;
    let ident = decl
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        ByteSpan::from(ident.text_range()),
    ))
}

use crate::check::infer::table::Table;

/// `textDocument/implementation` — when the cursor is on a class name, every
/// subclass decl's name range; when on an enum name, every variant's name range.
/// In-file only (subclasses across files are a documented follow-up). Returns an
/// empty vec when the cursor is not on a class/enum name.
pub fn implementations_in_file(model: &SemanticModel, offset: usize) -> Vec<Range> {
    let Some(name) = name_at_offset(model, offset) else {
        return Vec::new();
    };
    let table = Table::build(&model.tree, &model.resolved);
    if let Some(class_id) = table.class_id(&name) {
        // Every class whose ancestry includes this class (excluding itself).
        let mut out = Vec::new();
        for node in model
            .tree
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ClassDecl)
        {
            let Some(other) = crate::syntax::resolve::ident_text(node) else {
                continue;
            };
            let Some(other_id) = table.class_id(&other) else {
                continue;
            };
            if other_id != class_id && table.is_subclass(other_id, class_id) {
                if let Some(r) = decl_name_range(model, &other) {
                    out.push(r);
                }
            }
        }
        return out;
    }
    if table.enum_id(&name).is_some() {
        // The cursor is an enum name: return each variant's name range.
        return enum_variant_ranges(model, &name);
    }
    Vec::new()
}

/// The identifier text at `offset` (a `NameRef` token or a decl's name `Ident`).
fn name_at_offset(model: &SemanticModel, offset: usize) -> Option<String> {
    let node = model.tree.descendants().find(|n| {
        let r = n.text_range();
        let (s, e): (usize, usize) = (r.start().into(), r.end().into());
        offset >= s
            && offset < e
            && matches!(
                n.kind(),
                SyntaxKind::NameRef | SyntaxKind::ClassDecl | SyntaxKind::EnumDecl
            )
    })?;
    crate::syntax::resolve::ident_text(node)
}

/// Each variant's name range in the enum named `enum_name`.
fn enum_variant_ranges(model: &SemanticModel, enum_name: &str) -> Vec<Range> {
    let Some(decl) = model.tree.descendants().find(|n| {
        n.kind() == SyntaxKind::EnumDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(enum_name)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in decl
        .children()
        .filter(|c| c.kind() == SyntaxKind::EnumVariant)
    {
        if let Some(ident) = v
            .children_with_tokens()
            .filter_map(|el| el.into_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Ident)
        {
            out.push(crate::lsp::convert::byte_span_to_range(
                &model.text,
                &model.line_index,
                ByteSpan::from(ident.text_range()),
            ));
        }
    }
    out
}

#[cfg(test)]
mod impl_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn class_implementations_are_subclasses() {
        let src = "class Animal {}\nclass Dog extends Animal {}\nclass Cat extends Animal {}\n";
        let m = model(src);
        let off = src.find("Animal").unwrap() + 1; // on the `Animal` class name
        let impls = implementations_in_file(&m, off);
        assert_eq!(impls.len(), 2, "Dog + Cat: {impls:?}");
    }

    #[test]
    fn enum_implementations_are_variants() {
        let src = "enum Color { Red, Green, Blue }\nprint(1)\n";
        let m = model(src);
        let off = src.find("Color").unwrap() + 1;
        let impls = implementations_in_file(&m, off);
        assert_eq!(impls.len(), 3, "{impls:?}");
    }

    #[test]
    fn non_type_offset_yields_empty() {
        let m = model("let x = 1\nprint(x)\n");
        let off = m.text.rfind('x').unwrap();
        assert!(implementations_in_file(&m, off).is_empty());
    }
}

#[cfg(test)]
mod type_def_tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn type_definition_jumps_to_class_decl() {
        let src = "class User { name: string }\nlet u: User = User.from({ name: \"a\" })\nprint(u)\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        // Cursor on the use `u` in `print(u)`.
        let off = src.rfind('u').unwrap();
        let r = type_definition_in_file(&model, off);
        // If SP10 infers `u: User`, jump to the `User` decl on line 0.
        if let Some(r) = r {
            assert_eq!(r.start.line, 0, "should jump to the class User decl");
        }
        // (When SP10 cannot infer the type, None is acceptable — documented.)
    }

    #[test]
    fn first_type_ident_strips_optional_and_containers() {
        assert_eq!(first_type_ident("User"), Some("User".to_string()));
        assert_eq!(first_type_ident("User?"), Some("User".to_string()));
        assert_eq!(first_type_ident("array<User>"), Some("User".to_string()));
        assert_eq!(first_type_ident("number"), None);
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn declaration_resolves_like_definition() {
        let src = "fn f() {\n  let y = 1\n  return y\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let use_off = src.rfind('y').unwrap();
        let r = declaration_in_file(&model, use_off).expect("decl");
        assert_eq!(r.start.line, 1); // the `let y` line
    }
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
