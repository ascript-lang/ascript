//! `textDocument/hover` over the model: declaration/keyword/builtin docs plus the
//! SP10 inferred/declared type.

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

/// Hover at byte `offset`. Returns the inferred/declared type (if any) plus a
/// keyword/builtin/declaration doc line. `None` when neither is available (cursor
/// on trivia / an unknown token with no inferred type).
pub fn hover(model: &SemanticModel, offset: usize) -> Option<Hover> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ty) = crate::check::infer::hover_type_at(&model.text, offset) {
        parts.push(format!("```ascript\n{ty}\n```"));
    }
    if let Some(doc) = super::docs::doc_at(model, offset) {
        parts.push(doc);
    }
    if parts.is_empty() {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n---\n\n"),
        }),
        range: None,
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
    fn hover_on_typed_let_shows_type() {
        let src = "let x: number = 1\nprint(x)\n";
        let m = model(src);
        let off = src.rfind('x').unwrap(); // the use in print(x)
        let h = hover(&m, off).expect("hover");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("number"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_builtin_print_mentions_print() {
        let src = "print(1)\n";
        let m = model(src);
        let off = src.find("print").unwrap();
        let h = hover(&m, off).expect("hover on print");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("print"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_keyword_fn() {
        let src = "fn foo() {}\n";
        let m = model(src);
        let h = hover(&m, 0).expect("hover on fn");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.to_lowercase().contains("function"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_whitespace_is_none() {
        let m = model("let x = 1\n");
        assert!(hover(&m, 3).is_none()); // the space
    }
}
