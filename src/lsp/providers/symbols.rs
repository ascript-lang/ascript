//! `textDocument/documentSymbol` over the CST.
//!
//! Walks the top-level declarations of the cached [`SemanticModel`]'s CST
//! (unwrapping a leading `export`) and emits [`DocumentSymbol`]s with nesting:
//! a `class` carries its fields (PROPERTY) and methods (METHOD) as children, in
//! source order (fields before methods, matching the parser), and an `enum`
//! carries its variants (ENUM_MEMBER). Mirrors the kinds + name extraction the
//! cross-file index uses (`workspace.rs::decl_kind`/`name_range_of`).

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{DocumentSymbol, Range, SymbolKind};

/// Top-level document symbols (functions, classes + their methods/fields, enums +
/// variants, lets/consts).
#[allow(deprecated)] // DocumentSymbol::deprecated field
pub fn document_symbols(model: &SemanticModel) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for child in model.tree.children() {
        // Unwrap a leading `export <decl>`.
        let decl = if child.kind() == SyntaxKind::ExportStmt {
            match child.children().next() {
                Some(d) => d,
                None => continue,
            }
        } else {
            child
        };
        if let Some(sym) = symbol_for(model, decl) {
            out.push(sym);
        }
    }
    out
}

/// The full range of `node` as an LSP `Range`.
fn full_range(model: &SemanticModel, node: &ResolvedNode) -> Range {
    crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        ByteSpan::from(node.text_range()),
    )
}

/// The NAME-token range of `node` (its first `Ident`), falling back to the full
/// range — mirrors `workspace.rs::name_range_of`.
fn name_range(model: &SemanticModel, node: &ResolvedNode) -> Range {
    let tr = node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text_range())
        .unwrap_or_else(|| node.text_range());
    crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, ByteSpan::from(tr))
}

/// Build a top-level symbol for a declaration node, or `None` if it binds nothing.
#[allow(deprecated)]
fn symbol_for(model: &SemanticModel, node: &ResolvedNode) -> Option<DocumentSymbol> {
    let (kind, children) = match node.kind() {
        SyntaxKind::FnDecl => (SymbolKind::FUNCTION, None),
        SyntaxKind::ClassDecl => (SymbolKind::CLASS, Some(class_children(model, node))),
        SyntaxKind::EnumDecl => (SymbolKind::ENUM, Some(enum_children(model, node))),
        SyntaxKind::LetStmt => (let_kind(node), None),
        _ => return None,
    };
    let name = crate::syntax::resolve::ident_text(node)?;
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: full_range(model, node),
        selection_range: name_range(model, node),
        children,
    })
}

/// `VARIABLE` for a `let`, `CONSTANT` for a `const` (detected by a `ConstKw`
/// child), mirroring `workspace.rs::decl_kind`.
fn let_kind(node: &ResolvedNode) -> SymbolKind {
    let is_const = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == SyntaxKind::ConstKw);
    if is_const {
        SymbolKind::CONSTANT
    } else {
        SymbolKind::VARIABLE
    }
}

/// A class's fields (PROPERTY) and methods (METHOD), in CST source order.
#[allow(deprecated)]
fn class_children(model: &SemanticModel, class: &ResolvedNode) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for member in class.children() {
        let kind = match member.kind() {
            SyntaxKind::FieldDecl => SymbolKind::PROPERTY,
            SyntaxKind::MethodDecl => SymbolKind::METHOD,
            _ => continue,
        };
        let Some(name) = crate::syntax::resolve::ident_text(member) else {
            continue;
        };
        out.push(DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: full_range(model, member),
            selection_range: name_range(model, member),
            children: None,
        });
    }
    out
}

/// An enum's variants (ENUM_MEMBER), in CST source order.
#[allow(deprecated)]
fn enum_children(model: &SemanticModel, enm: &ResolvedNode) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for variant in enm.children() {
        if variant.kind() != SyntaxKind::EnumVariant {
            continue;
        }
        let Some(name) = crate::syntax::resolve::ident_text(variant) else {
            continue;
        };
        out.push(DocumentSymbol {
            name,
            detail: None,
            kind: SymbolKind::ENUM_MEMBER,
            tags: None,
            deprecated: None,
            range: full_range(model, variant),
            selection_range: name_range(model, variant),
            children: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn lists_top_level_decls() {
        let syms =
            document_symbols(&model("fn foo() {}\nclass C {}\nenum E { A, B }\nlet v = 1\n"));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "{names:?}");
        assert!(names.contains(&"C"), "{names:?}");
        assert!(names.contains(&"E"), "{names:?}");
        assert!(names.contains(&"v"), "{names:?}");
    }

    #[test]
    #[allow(deprecated)]
    fn nests_class_fields_before_methods_and_enum_variants() {
        let src = "class Point {\n  x: number\n  label: string?\n  fn init() {}\n}\nenum E { A, B }\nexport fn bar() {}\nconst K = 1\n";
        let syms = document_symbols(&model(src));
        // exported decl unwrapped.
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"bar"), "{names:?}");

        let point = syms.iter().find(|s| s.name == "Point").expect("Point");
        assert_eq!(point.kind, SymbolKind::CLASS);
        let children = point.children.as_ref().expect("class children");
        let cnames: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cnames, vec!["x", "label", "init"]);
        assert_eq!(children[0].kind, SymbolKind::PROPERTY);
        assert_eq!(children[1].kind, SymbolKind::PROPERTY);
        assert_eq!(children[2].kind, SymbolKind::METHOD);

        let e = syms.iter().find(|s| s.name == "E").expect("E");
        let variants = e.children.as_ref().expect("enum children");
        let vnames: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(vnames, vec!["A", "B"]);
        assert!(variants.iter().all(|v| v.kind == SymbolKind::ENUM_MEMBER));

        let k = syms.iter().find(|s| s.name == "K").expect("K");
        assert_eq!(k.kind, SymbolKind::CONSTANT);
    }
}
