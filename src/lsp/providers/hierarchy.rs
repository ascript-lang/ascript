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
/// edges), excluding the declaration itself.
pub fn incoming_calls(
    idx: &WorkspaceIndex,
    anchor: &CallAnchor,
) -> Vec<(PathBuf, crate::check::diagnostic::ByteSpan)> {
    let off = anchor.name_range.start;
    idx.references_at(&anchor.path, off, false)
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
        let call_site = crate::check::diagnostic::ByteSpan::from(callee.text_range());
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
        assert!(outs.iter().any(|o| o.name == "a"), "{outs:?}");
    }
}
