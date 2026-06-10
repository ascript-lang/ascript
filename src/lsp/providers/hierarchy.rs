//! `callHierarchy` and `typeHierarchy` — cross-file structural navigation over the
//! `WorkspaceIndex` (call graph) + the CST class/enum `Table` (type graph). Pure
//! functions; the server adapts them to the LSP wire types.

use crate::lsp::workspace::WorkspaceIndex;
use std::path::{Path, PathBuf};

/// A resolved call-hierarchy anchor: the canonical defining file + the callable's
/// name + its name-range (byte span). The handler maps this to a
/// `CallHierarchyItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAnchor {
    pub path: PathBuf,
    pub name: String,
    pub name_range: crate::check::diagnostic::ByteSpan,
}

/// Resolve the cursor in `path` at byte `offset` to its call-hierarchy anchor (the
/// canonical definition it refers to), via the index's `def_at`.
pub fn prepare_call(idx: &WorkspaceIndex, path: &Path, offset: usize) -> Option<CallAnchor> {
    let (def_path, name, name_range) = idx.def_at(path, offset)?;
    Some(CallAnchor {
        path: def_path,
        name,
        name_range,
    })
}

/// Incoming calls: every reference to the anchor across the workspace, grouped by
/// the file they occur in. Reuses `references_at` (which already follows import
/// edges, excluding the declaration itself) and then FILTERS to actual call sites
/// — a reference that merely uses the callable as a value (`let g = helper`) is
/// dropped; only references that are the CALLEE `NameRef` of a `CallExpr` survive.
pub fn incoming_calls(
    idx: &WorkspaceIndex,
    anchor: &CallAnchor,
) -> Vec<(PathBuf, crate::check::diagnostic::ByteSpan)> {
    let off = anchor.name_range.start;
    idx.references_at(&anchor.path, off, false)
        .into_iter()
        .filter(|(path, span)| is_call_site(idx, path, *span))
        .collect()
}

/// True iff the reference at `span` in `path` is the CALLEE of a `CallExpr` (vs. a
/// bare value use). Re-parses the file's cached index text (the established pattern
/// from `file_module_arity`/`exported_fn_arity`) and locates the `NameRef` whose
/// range matches `span`; it is a call site iff its parent is a `CallExpr` and it is
/// that `CallExpr`'s first `NameRef` child (the callee position).
fn is_call_site(
    idx: &WorkspaceIndex,
    path: &Path,
    span: crate::check::diagnostic::ByteSpan,
) -> bool {
    use crate::syntax::kind::SyntaxKind;
    let canon = crate::lsp::workspace::canon(path);
    let Some(file) = idx.files.get(&canon) else {
        return false;
    };
    let parsed = crate::syntax::parser::parse(&file.text);
    if !parsed.errors.is_empty() || !parsed.lex_errors.is_empty() {
        return false;
    }
    let tree = crate::syntax::tree_builder::build_tree(parsed);
    // The NameRef whose bare `Ident` TOKEN range is exactly the reference span.
    // `UseSite.range` (what `references_at` returns) is the use TOKEN range, NOT the
    // NameRef NODE range — a NameRef node can carry leading whitespace trivia (DX
    // D3-T12), so we match the Ident token, mirroring `collect_uses`.
    let Some(name_ref) = tree.descendants().find(|n| {
        n.kind() == SyntaxKind::NameRef
            && n.children_with_tokens()
                .filter_map(|el| el.into_token())
                .find(|t| t.kind() == SyntaxKind::Ident)
                .map(|t| crate::check::diagnostic::ByteSpan::from(t.text_range()) == span)
                .unwrap_or(false)
    }) else {
        return false;
    };
    let Some(parent) = name_ref.parent() else {
        return false;
    };
    if parent.kind() != SyntaxKind::CallExpr {
        return false;
    }
    // It is the callee iff it is the CallExpr's first NameRef child (the callee
    // position — mirrors `file_module_arity`'s `children().find(NameRef)`).
    let callee_range = parent
        .children()
        .find(|c| c.kind() == SyntaxKind::NameRef)
        .map(|c| c.text_range());
    callee_range == Some(name_ref.text_range())
}

#[cfg(test)]
mod call_tests {
    use super::*;
    use std::fs;

    fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, WorkspaceIndex) {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();
        for (name, src) in files {
            let p = dir.path().join(name);
            fs::write(&p, src).unwrap();
            entries.push((p, src.to_string()));
        }
        (dir, WorkspaceIndex::build_from_files(&entries))
    }

    #[test]
    fn prepare_resolves_a_callee() {
        let (dir, idx) = index(&[(
            "a.as",
            "fn helper() { return 1 }\nfn main() { return helper() }\n",
        )]);
        let p = crate::lsp::workspace::canon(&dir.path().join("a.as"));
        let text = &idx.files[&p].text;
        let off = text.find("helper()").unwrap(); // the call site
        let anchor = prepare_call(&idx, &p, off).expect("anchor");
        assert_eq!(anchor.name, "helper");
        assert_eq!(
            &text[anchor.name_range.start..anchor.name_range.end],
            "helper"
        );
    }

    #[test]
    fn incoming_finds_the_call_site() {
        let (dir, idx) = index(&[(
            "a.as",
            "fn helper() { return 1 }\nfn main() { return helper() }\n",
        )]);
        let p = crate::lsp::workspace::canon(&dir.path().join("a.as"));
        let text = &idx.files[&p].text;
        let decl_off = text.find("fn helper").unwrap() + 3;
        let anchor = prepare_call(&idx, &p, decl_off).expect("anchor on decl");
        let incoming = incoming_calls(&idx, &anchor);
        assert!(
            !incoming.is_empty(),
            "should find the call in main: {incoming:?}"
        );
    }

    #[test]
    fn incoming_excludes_value_use_references() {
        // `helper` is BOTH called (`helper()`) AND used as a value (`let g = helper`).
        // Only the call site is an incoming call; the value-use reference must NOT
        // appear.
        let src = "fn helper() { return 1 }\nfn main() {\n  let g = helper\n  return helper()\n}\n";
        let (dir, idx) = index(&[("a.as", src)]);
        let p = crate::lsp::workspace::canon(&dir.path().join("a.as"));
        let text = &idx.files[&p].text;
        let decl_off = text.find("fn helper").unwrap() + 3;
        let anchor = prepare_call(&idx, &p, decl_off).expect("anchor on decl");
        let incoming = incoming_calls(&idx, &anchor);
        assert_eq!(
            incoming.len(),
            1,
            "exactly the one call site, not the value use: {incoming:?}"
        );
        // The single ref is the CALL-site `helper`, not the `let g = helper` one:
        // its span lands in the `return helper()` region, AFTER the value use.
        let (_, span) = &incoming[0];
        let value_use_off = text.find("= helper").unwrap();
        let call_off = text.rfind("helper").unwrap();
        assert!(
            span.start > value_use_off && span.start <= call_off,
            "ref is the call-site occurrence, not the value use: {span:?}"
        );
        assert!(
            text[span.start..span.end].contains("helper"),
            "span covers the `helper` identifier: {span:?}"
        );
    }
}

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;

/// An outgoing call target: the callee name + the call-site range (in the caller's
/// file) + the resolved definition location, if the index can resolve it.
#[derive(Debug, Clone)]
pub struct OutgoingCall {
    pub name: String,
    pub call_site: crate::check::diagnostic::ByteSpan,
    pub def: Option<(PathBuf, crate::check::diagnostic::ByteSpan)>,
}

/// Outgoing calls from the function whose body contains `anchor.name_range`: every
/// `CallExpr` with a `NameRef` callee inside that fn's CST node, resolved against
/// the index. `model` is the anchor file's cached model.
pub fn outgoing_calls(
    idx: &WorkspaceIndex,
    model: &SemanticModel,
    anchor: &CallAnchor,
) -> Vec<OutgoingCall> {
    // Find the FnDecl/MethodDecl node whose name range matches the anchor.
    let Some(fn_node) = model.tree.descendants().find(|n| {
        matches!(n.kind(), SyntaxKind::FnDecl | SyntaxKind::MethodDecl)
            && fn_name_range(n) == Some(anchor.name_range)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for call in fn_node
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(callee) else {
            continue;
        };
        // The callee's bare `Ident` TOKEN range — NOT the NameRef NODE range, which
        // can carry leading whitespace trivia. `definition_at` resolves a use by the
        // TOKEN-range `UseSite` (DX D3-T12), so a trivia-inclusive start would land
        // before the use and silently return `def == None` for every trivia-preceded
        // callee (the same class of bug fixed for `is_call_site`).
        let call_site = callee
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::Ident)
            .map(|t| crate::check::diagnostic::ByteSpan::from(t.text_range()))
            .unwrap_or_else(|| crate::check::diagnostic::ByteSpan::from(callee.text_range()));
        let def = idx.definition_at(&anchor.path, call_site.start);
        out.push(OutgoingCall {
            name,
            call_site,
            def,
        });
    }
    out
}

/// The name-range (byte span) of a `FnDecl`/`MethodDecl`'s name `Ident`.
fn fn_name_range(
    node: &crate::syntax::cst::ResolvedNode,
) -> Option<crate::check::diagnostic::ByteSpan> {
    let ident = node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::check::diagnostic::ByteSpan::from(ident.text_range()))
}

use crate::check::infer::table::Table;

/// A resolved type-hierarchy anchor: the class/enum name + its decl name-range in
/// the file it is declared in (in-file resolution; cross-file extends is a
/// follow-up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAnchor {
    pub name: String,
    pub name_range: crate::check::diagnostic::ByteSpan,
    pub is_class: bool,
}

/// Resolve the cursor at `offset` to a class/enum type anchor in `model`.
pub fn prepare_type(model: &SemanticModel, offset: usize) -> Option<TypeAnchor> {
    let name = type_name_at(model, offset)?;
    let table = Table::build(&model.tree, &model.resolved);
    let (is_class, decl_kind) = if table.class_id(&name).is_some() {
        (true, SyntaxKind::ClassDecl)
    } else if table.enum_id(&name).is_some() {
        (false, SyntaxKind::EnumDecl)
    } else {
        return None;
    };
    let range = decl_name_byte_range(model, &name, decl_kind)?;
    Some(TypeAnchor {
        name,
        name_range: range,
        is_class,
    })
}

/// The supertype names (the `extends` chain, nearest first) of the class `name`.
pub fn supertypes(
    model: &SemanticModel,
    name: &str,
) -> Vec<(String, crate::check::diagnostic::ByteSpan)> {
    let table = Table::build(&model.tree, &model.resolved);
    let mut out = Vec::new();
    let mut cur = table.class_id(name);
    let mut visited = Vec::new();
    while let Some(id) = cur {
        if visited.contains(&id) {
            break;
        }
        visited.push(id);
        let Some(ci) = table.class(id) else { break };
        let Some(parent) = ci.parent else { break };
        let Some(pinfo) = table.class(parent) else {
            break;
        };
        if let Some(r) = decl_name_byte_range(model, &pinfo.name, SyntaxKind::ClassDecl) {
            out.push((pinfo.name.clone(), r));
        }
        cur = Some(parent);
    }
    out
}

/// The direct-subtype names of the class `name` (every class whose immediate
/// parent is `name`).
pub fn subtypes(
    model: &SemanticModel,
    name: &str,
) -> Vec<(String, crate::check::diagnostic::ByteSpan)> {
    let table = Table::build(&model.tree, &model.resolved);
    let Some(target) = table.class_id(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ClassDecl)
    {
        let Some(other) = crate::syntax::resolve::ident_text(node) else {
            continue;
        };
        let Some(oid) = table.class_id(&other) else {
            continue;
        };
        if let Some(ci) = table.class(oid) {
            if ci.parent == Some(target) {
                if let Some(r) = decl_name_byte_range(model, &other, SyntaxKind::ClassDecl) {
                    out.push((other, r));
                }
            }
        }
    }
    out
}

/// The identifier text at `offset` if it is a class/enum NAME or NameRef.
fn type_name_at(model: &SemanticModel, offset: usize) -> Option<String> {
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

/// The byte span of the name `Ident` of the `kind` decl named `name`.
fn decl_name_byte_range(
    model: &SemanticModel,
    name: &str,
    kind: SyntaxKind,
) -> Option<crate::check::diagnostic::ByteSpan> {
    let decl = model.tree.descendants().find(|n| {
        n.kind() == kind && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    })?;
    let ident = decl
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::check::diagnostic::ByteSpan::from(ident.text_range()))
}

#[cfg(test)]
mod type_hierarchy_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn supertypes_walk_the_extends_chain() {
        let src = "class A {}\nclass B extends A {}\nclass C extends B {}\n";
        let m = model(src);
        let sup = supertypes(&m, "C");
        let names: Vec<&str> = sup.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["B", "A"]);
    }

    #[test]
    fn subtypes_are_direct_children() {
        let src = "class A {}\nclass B extends A {}\nclass C extends A {}\n";
        let m = model(src);
        let sub = subtypes(&m, "A");
        let mut names: Vec<&str> = sub.iter().map(|(n, _)| n.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["B", "C"]);
    }

    #[test]
    fn prepare_resolves_a_class() {
        let m = model("class A {}\nclass B extends A {}\n");
        let off = m.text.find("class A").unwrap() + 6;
        let anchor = prepare_type(&m, off).expect("anchor");
        assert_eq!(anchor.name, "A");
        assert!(anchor.is_class);
    }

    #[test]
    fn prepare_resolves_an_enum_with_empty_hierarchy() {
        let m = model("enum Color { Red, Green }\n");
        let off = m.text.find("Color").unwrap() + 1;
        let anchor = prepare_type(&m, off).expect("enum anchor");
        assert_eq!(anchor.name, "Color");
        assert!(!anchor.is_class);
        assert!(supertypes(&m, "Color").is_empty());
        assert!(subtypes(&m, "Color").is_empty());
    }
}

#[cfg(test)]
mod outgoing_tests {
    use super::*;
    use crate::check::LintConfig;
    use std::fs;

    #[test]
    fn outgoing_lists_inner_calls() {
        let dir = tempfile::tempdir().unwrap();
        let src = "fn a() { return 1 }\nfn main() { return a() }\n";
        let p = dir.path().join("a.as");
        fs::write(&p, src).unwrap();
        let idx = WorkspaceIndex::build_from_files(&[(p.clone(), src.to_string())]);
        let canon = crate::lsp::workspace::canon(&p);
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let main_off = src.find("fn main").unwrap() + 3;
        let anchor = prepare_call(&idx, &canon, main_off).expect("anchor on main");
        let outs = outgoing_calls(&idx, &model, &anchor);
        let call = outs.iter().find(|o| o.name == "a").expect("outgoing call `a`");
        // The callee `a` in `return a()` has leading trivia; its def must still
        // resolve (regression guard: a trivia-inclusive call-site offset returned
        // `def == None` because `UseSite.range` is the Ident TOKEN range, T12).
        assert!(
            call.def.is_some(),
            "outgoing call `a` must resolve its definition: {outs:?}"
        );
        // And the reported call-site span is the bare token (1 char `a`), not the
        // NameRef node + leading space.
        let span = call.call_site;
        assert_eq!(span.end - span.start, 1, "call_site should be the `a` token: {span:?}");
    }
}
