//! `textDocument/signatureHelp`: while the cursor is inside a call's argument
//! list, surface the callee's parameter list with the active parameter
//! highlighted. In-file only (cross-file deferred to Phase 3's hierarchy work) —
//! the callee must resolve to a unique in-file `FnDecl`. A method-call callee
//! (`obj.m(...)`, whose callee is a `MemberExpr` rather than a `NameRef`) returns
//! `None`: a documented v1 limitation.

use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};

/// Signature help at byte `offset`, or `None` when the cursor is not inside a
/// resolvable call's argument list.
pub fn signature_help(model: &SemanticModel, offset: usize) -> Option<SignatureHelp> {
    let (callee_name, arg_list) = enclosing_call(model, offset)?;
    let fn_decl = find_fn_decl(model, &callee_name)?;
    let params = param_names(&fn_decl);
    if params.is_empty() {
        // Still offer a zero-arg signature label.
        return Some(make_help(&callee_name, &[], 0));
    }
    let active = active_param_index(&arg_list, offset);
    Some(make_help(&callee_name, &params, active))
}

/// The callee name + the `ArgList` node of the INNERMOST `CallExpr` whose arg
/// list span contains `offset`.
fn enclosing_call(model: &SemanticModel, offset: usize) -> Option<(String, ResolvedNode)> {
    let mut best: Option<(String, ResolvedNode, usize)> = None; // (name, arglist, span_len)
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(arg_list) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let r = arg_list.text_range();
        let (s, e) = (usize::from(r.start()), usize::from(r.end()));
        if offset >= s && offset <= e {
            let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
                continue;
            };
            let Some(name) = crate::syntax::resolve::ident_text(callee) else {
                continue;
            };
            let len = e - s;
            if best.as_ref().map(|b| len < b.2).unwrap_or(true) {
                best = Some((name, arg_list.clone(), len));
            }
        }
    }
    best.map(|(n, a, _)| (n, a))
}

/// The unique in-file `FnDecl` named `name`, if exactly one exists.
fn find_fn_decl(model: &SemanticModel, name: &str) -> Option<ResolvedNode> {
    let mut found = model.tree.descendants().filter(|n| {
        n.kind() == SyntaxKind::FnDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    });
    let first = found.next()?.clone();
    if found.next().is_some() {
        return None; // ambiguous — skip (zero-FP)
    }
    Some(first)
}

/// Param NAMES of a `FnDecl` (each `Param`'s `Ident` text, in order).
fn param_names(fn_decl: &ResolvedNode) -> Vec<String> {
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

/// Active parameter index = the count of top-level `Comma` tokens in `arg_list`
/// that occur at a byte position < `offset`.
fn active_param_index(arg_list: &ResolvedNode, offset: usize) -> u32 {
    let mut commas = 0u32;
    for el in arg_list.children_with_tokens() {
        if let Some(tok) = el.into_token() {
            if tok.kind() == SyntaxKind::Comma {
                let pos = usize::from(tok.text_range().start());
                if pos < offset {
                    commas += 1;
                }
            }
        }
    }
    commas
}

fn make_help(name: &str, params: &[String], active: u32) -> SignatureHelp {
    let label = format!("{name}({})", params.join(", "));
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
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
    fn shows_signature_and_first_param() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        // Cursor right after `add(` — inside the arg list, before any comma.
        let off = src.rfind("add(").unwrap() + "add(".len();
        let help = signature_help(&m, off).expect("help");
        assert_eq!(help.signatures[0].label, "add(a, b)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn active_param_advances_past_comma() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        // Cursor after the comma (on `2`).
        let off = src.rfind('2').unwrap();
        let help = signature_help(&m, off).expect("help");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn none_outside_a_call() {
        let m = model("let x = 1\n");
        assert!(signature_help(&m, 4).is_none());
    }
}
