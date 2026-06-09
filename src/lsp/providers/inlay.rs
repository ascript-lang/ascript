//! `textDocument/inlayHint` (+ resolve): inferred-type hints at un-annotated
//! `let`/`const` sites, and parameter-name hints at call arguments. Types come
//! from the SP10 inferencer (`check::infer::hover_type_at`); names from the
//! callee's in-file `FnDecl` param list. Hints in a requested range only.

use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};

/// Inlay hints whose position falls within `range` (byte-filtered after build).
pub fn inlay_hints(model: &SemanticModel, range: Range) -> Vec<InlayHint> {
    let lo = crate::lsp::convert::char_to_byte(&model.text, model.line_index.offset(range.start));
    let hi = crate::lsp::convert::char_to_byte(&model.text, model.line_index.offset(range.end));
    let mut out = Vec::new();
    out.extend(type_hints(model));
    out.extend(param_name_hints(model));
    out.into_iter()
        .filter(|h| {
            let b =
                crate::lsp::convert::char_to_byte(&model.text, model.line_index.offset(h.position));
            b >= lo && b <= hi
        })
        .collect()
}

/// Inferred-type hints at un-annotated `let`/`const` bindings: `let x⟦: number⟧`.
fn type_hints(model: &SemanticModel) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for stmt in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LetStmt)
    {
        // Skip if already annotated (a `Colon` direct token child).
        let annotated = stmt
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == SyntaxKind::Colon);
        if annotated {
            continue;
        }
        // The binding's NAME token (first Ident token in the stmt).
        let Some(name_tok) = stmt
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::Ident)
        else {
            continue;
        };
        let name_end = usize::from(name_tok.text_range().end());
        let name_start = usize::from(name_tok.text_range().start());
        // Inferred type at the binding NAME (the SP10 pass records an un-annotated
        // `let`/`const` binding's inferred type on its name-token range in hover mode).
        let Some(ty) = crate::check::infer::hover_type_at(&model.text, name_start) else {
            continue;
        };
        // Don't emit a noise hint for an unknown/`any` type.
        if ty == "any" {
            continue;
        }
        let pos = model
            .line_index
            .position(crate::lsp::convert::byte_to_char(&model.text, name_end));
        out.push(type_hint(pos, &ty));
    }
    out
}

/// Parameter-name hints: `f(⟦a:⟧ 1, ⟦b:⟧ 2)`.
fn param_name_hints(model: &SemanticModel) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(callee) else {
            continue;
        };
        let Some(fn_decl) = find_unique_fn_decl(model, &name) else {
            continue;
        };
        let params = fn_param_names(&fn_decl);
        let Some(arg_list) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        // A spread arg (`f(...xs, 3)`) breaks the positional-index→param mapping (the
        // spread expands to an unknown count), so the trailing args would be labeled
        // with the WRONG param name. Suppress param-name hints for the whole call —
        // matching the checker's `call-arity`/`contract` spread bail-out.
        if arg_list
            .children()
            .any(|c| c.kind() == SyntaxKind::SpreadElem)
        {
            continue;
        }
        // Positional argument expressions, in order.
        let args: Vec<&ResolvedNode> = arg_list
            .children()
            .filter(|c| crate::check::rules::is_expr_kind(c.kind()))
            .collect();
        for (i, arg) in args.iter().enumerate() {
            let Some(pname) = params.get(i) else { break };
            let arg_start = usize::from(arg.text_range().start());
            let pos = model
                .line_index
                .position(crate::lsp::convert::byte_to_char(&model.text, arg_start));
            out.push(param_hint(pos, pname));
        }
    }
    out
}

fn find_unique_fn_decl(model: &SemanticModel, name: &str) -> Option<ResolvedNode> {
    let mut it = model.tree.descendants().filter(|n| {
        n.kind() == SyntaxKind::FnDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    });
    let first = it.next()?.clone();
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

fn fn_param_names(fn_decl: &ResolvedNode) -> Vec<String> {
    let Some(list) = fn_decl
        .children()
        .find(|c| c.kind() == SyntaxKind::ParamList)
    else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == SyntaxKind::Param)
        .filter_map(crate::syntax::resolve::ident_text)
        .collect()
}

fn type_hint(pos: Position, ty: &str) -> InlayHint {
    InlayHint {
        position: pos,
        label: InlayHintLabel::String(format!(": {ty}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(false),
        data: None,
    }
}

fn param_hint(pos: Position, name: &str) -> InlayHint {
    InlayHint {
        position: pos,
        label: InlayHintLabel::String(format!("{name}:")),
        kind: Some(InlayHintKind::PARAMETER),
        text_edits: None,
        tooltip: None,
        padding_left: Some(false),
        padding_right: Some(true),
        data: None,
    }
}

/// `inlayHint/resolve`: attach a tooltip lazily. v1 reflects the label into a
/// plain-string tooltip (the heavy detail computation hook for later).
pub fn resolve(hint: InlayHint) -> InlayHint {
    let mut h = hint;
    if h.tooltip.is_none() {
        if let InlayHintLabel::String(s) = &h.label {
            h.tooltip = Some(tower_lsp::lsp_types::InlayHintTooltip::String(s.clone()));
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    fn full_range(m: &SemanticModel) -> Range {
        let end = m.line_index.position(m.text.chars().count());
        Range::new(Position::new(0, 0), end)
    }

    #[test]
    fn type_hint_on_unannotated_let() {
        // NUM §5: an integer literal synths the concrete `int` subtype, so the inlay
        // hint for `let n = 1` is `: int` (was `: number` before the numeric model).
        let src = "let n = 1\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let type_hints: Vec<&InlayHint> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::TYPE))
            .collect();
        assert!(!type_hints.is_empty(), "expected a type hint, got {hints:?}");
        if let InlayHintLabel::String(s) = &type_hints[0].label {
            assert!(s.contains("int"), "got {s}");
        } else {
            panic!("expected a string label");
        }
    }

    #[test]
    fn type_hint_on_unannotated_float_let() {
        // A float literal synths `float`.
        let src = "let n = 1.5\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let type_hints: Vec<&InlayHint> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::TYPE))
            .collect();
        assert!(!type_hints.is_empty(), "expected a type hint, got {hints:?}");
        if let InlayHintLabel::String(s) = &type_hints[0].label {
            assert!(s.contains("float"), "got {s}");
        } else {
            panic!("expected a string label");
        }
    }

    #[test]
    fn type_hint_surfaces_instantiated_generic() {
        // TYPE Task 16: the inlay type hint on an un-annotated binding of a generic
        // construction surfaces the SOLVED type args (`: Box<int>`), so the editor
        // shows the instantiated generic inline.
        let src = "class Box<T> {\n  value: T\n}\nlet b = Box(5)\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let labels: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::TYPE))
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels.iter().any(|s| s.contains("Box<int>")),
            "expected a `Box<int>` type hint, got {labels:?}"
        );
    }

    #[test]
    fn no_type_hint_when_annotated() {
        let m = model("let n: number = 1\n");
        let hints = inlay_hints(&m, full_range(&m));
        assert!(
            hints.iter().all(|h| h.kind != Some(InlayHintKind::TYPE)),
            "annotated let must not get a type hint"
        );
    }

    #[test]
    fn parameter_name_hints_at_call_args() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let names: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"a:".to_string()), "{names:?}");
        assert!(names.contains(&"b:".to_string()), "{names:?}");
    }

    #[test]
    fn no_param_hints_when_call_has_a_spread_arg() {
        // `f(...xs, 3)`: the spread arg shifts the positional index, so `3` would be
        // mislabeled `a:` instead of `c:`. Param-name hints are suppressed for any
        // call whose arg list contains a spread.
        let src = "fn f(a, b, c) { return a }\nlet xs = [1, 2]\nf(...xs, 3)\n";
        let m = model(src);
        let hints = inlay_hints(&m, full_range(&m));
        let names: Vec<String> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            names.is_empty(),
            "spread call must emit NO param-name hints, got {names:?}"
        );
        // The inferred-type hint on the `let xs` is unaffected.
        assert!(
            hints.iter().any(|h| h.kind == Some(InlayHintKind::TYPE)),
            "type hint on `let xs` still expected"
        );
    }

    #[test]
    fn resolve_fills_tooltip() {
        let h = type_hint(Position::new(0, 5), "number");
        let r = resolve(h);
        assert!(r.tooltip.is_some());
    }
}
