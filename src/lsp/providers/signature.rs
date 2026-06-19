//! `textDocument/signatureHelp`: while the cursor is inside a call's argument
//! list, surface the callee's parameter list with the active parameter
//! highlighted.
//!
//! Resolution ladder (first match wins):
//! 1. Simple-name callee (`f(…)`):
//!    a. Unique in-file `FnDecl` named `f`.
//!    b. Global builtin (`print`, `len`, …).
//!    c. Cross-file fn if `index` + `doc_path` are provided.
//! 2. Member callee (`recv.prop(…)`):
//!    a. `recv` is a namespace-import alias → stdlib `std_sig(module, prop)`.
//!    b. `recv` is a typed local → find the in-file class method `prop`.

use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{
    Documentation, ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Signature help at byte `offset`, or `None` when the cursor is not inside a
/// resolvable call's argument list.
pub fn signature_help(
    model: &SemanticModel,
    offset: usize,
    index: Option<&crate::lsp::workspace::WorkspaceIndex>,
    doc_path: Option<&std::path::Path>,
) -> Option<SignatureHelp> {
    let (callee, arg_list) = enclosing_call(model, offset)?;
    let active_raw = active_param_index(&arg_list, offset);

    match &callee {
        Callee::Named(name) => resolve_named(model, name, active_raw, index, doc_path),
        Callee::Member { receiver, property } => {
            resolve_member(model, receiver, property, active_raw)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Callee type
// ─────────────────────────────────────────────────────────────────────────────

enum Callee {
    Named(String),
    Member { receiver: String, property: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Resolution — Named callee
// ─────────────────────────────────────────────────────────────────────────────

fn resolve_named(
    model: &SemanticModel,
    name: &str,
    active_raw: u32,
    index: Option<&crate::lsp::workspace::WorkspaceIndex>,
    doc_path: Option<&std::path::Path>,
) -> Option<SignatureHelp> {
    // a. Same-file unique FnDecl (wins over builtin shadow).
    if let Some(fn_decl) = find_fn_decl(model, name) {
        let params = param_infos(&fn_decl);
        let active = clamp_active(active_raw, &params);
        return Some(make_help_user(name, &params, active));
    }

    // b. Global builtin.
    if let Some(sig) = crate::check::std_sigs::builtin_sig(name) {
        let active = clamp_active_std(active_raw, sig.params);
        let (label, offsets) = render_sig_label(name, sig);
        return Some(make_help_std(&label, offsets, sig, active));
    }

    // c. Cross-file indexed fn.
    if let (Some(idx), Some(path)) = (index, doc_path) {
        if let Some(sig) = idx.exported_fn_signature_by_import(model, path, name) {
            let active = clamp_active_exported(active_raw, &sig.params);
            return Some(make_help_exported(name, &sig, active));
        }
    }

    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Resolution — Member callee
// ─────────────────────────────────────────────────────────────────────────────

fn resolve_member(
    model: &SemanticModel,
    receiver: &str,
    property: &str,
    active_raw: u32,
) -> Option<SignatureHelp> {
    // a. Namespace import alias → stdlib.
    if let Some(module) = crate::lsp::providers::completion::namespace_import_module_pub(
        &model.text,
        receiver,
    ) {
        let sig = crate::check::std_sigs::std_sig(&module, property)?;
        let active = clamp_active_std(active_raw, sig.params);
        let prefix = format!("{receiver}.{property}");
        let (label, offsets) = render_sig_label(&prefix, sig);
        return Some(make_help_std(&label, offsets, sig, active));
    }

    // b. Typed local receiver → in-file class method.
    if let Some(help) = resolve_typed_receiver_method(model, receiver, property, active_raw) {
        return Some(help);
    }

    None
}

/// Find a method `property` on the class that `receiver` resolves to (only when
/// the receiver is a typed local with a definitively known class).
fn resolve_typed_receiver_method(
    model: &SemanticModel,
    receiver: &str,
    property: &str,
    active_raw: u32,
) -> Option<SignatureHelp> {
    // Build the char slice for receiver-class resolution.
    let chars: Vec<char> = model.text.chars().collect();
    // Find the byte offset of the receiver identifier in the source.
    // We look for `receiver.property(` pattern to locate the receiver's position.
    let pattern = format!("{receiver}.{property}(");
    let byte_pos = model.text.find(&pattern)?;
    let char_pos = model.text[..byte_pos + receiver.len()].chars().count();
    // Use the middle of the receiver name for type inference.
    let recv_mid_char = char_pos.saturating_sub(receiver.len() / 2 + 1);
    let byte_off =
        crate::lsp::convert::char_to_byte(&model.text, recv_mid_char.min(chars.len()));

    let rendered = crate::check::infer::hover_type_at(&model.text, byte_off)?;
    let class_name = first_class_ident(&rendered)?;

    // Find the unique FnDecl (method) named `property` inside the class `class_name`.
    let method_decl = find_method_decl(model, &class_name, property)?;
    let params = param_infos(&method_decl);
    let active = clamp_active(active_raw, &params);
    Some(make_help_user(property, &params, active))
}

/// Find a `MethodDecl` named `method` inside the `ClassDecl` named `class_name`.
fn find_method_decl(
    model: &SemanticModel,
    class_name: &str,
    method: &str,
) -> Option<ResolvedNode> {
    for class in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ClassDecl)
    {
        if crate::syntax::resolve::ident_text(class).as_deref() != Some(class_name) {
            continue;
        }
        // Walk class body for MethodDecl with matching name.
        for m_decl in class
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::MethodDecl)
        {
            if crate::syntax::resolve::ident_text(m_decl).as_deref() == Some(method) {
                return Some(m_decl.clone());
            }
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Param extraction from CST
// ─────────────────────────────────────────────────────────────────────────────

/// Information about one user-defined parameter.
struct ParamInfo {
    /// Display text, e.g. `count: int` or `...rest: array<string>`.
    label: String,
    _variadic: bool,
    _optional: bool,
}

/// Extract rich param info from a `FnDecl` or `MethodDecl` CST node.
fn param_infos(fn_decl: &ResolvedNode) -> Vec<ParamInfo> {
    use crate::check::rules::is_type_kind;
    use SyntaxKind::*;
    let Some(list) = fn_decl.children().find(|c| c.kind() == ParamList) else {
        return Vec::new();
    };
    list.children()
        .filter(|c| c.kind() == Param)
        .filter_map(|p| {
            // Is this a rest param?
            let variadic = p
                .children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == DotDotDot);

            // Param name.
            let name = crate::syntax::resolve::ident_text(p)?;

            // Optional: has a default expression child.
            let has_default = p
                .children()
                .any(|c| crate::check::rules::is_expr_kind(c.kind()));

            // Type annotation text: first type-kind child. Trim leading/trailing
            // whitespace that can appear as trivia in the CST text range.
            let ty_text: Option<String> = p
                .children()
                .find(|c| is_type_kind(c.kind()))
                .map(|t| t.text().to_string().trim().to_string());

            // Build label.
            let label = if let Some(ty) = ty_text {
                if variadic {
                    format!("...{name}: {ty}")
                } else {
                    format!("{name}: {ty}")
                }
            } else if variadic {
                format!("...{name}")
            } else {
                name.clone()
            };

            Some(ParamInfo { label, _variadic: variadic, _optional: has_default })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared render helper (feature-independent, no lsp_types imports)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a signature label string and per-parameter UTF-16 label offsets from a
/// prefix (e.g. `"math.pow"` or `"print"`) and a `StdSig`. Returns
/// `(label_string, Vec<[start_u16, end_u16]>)`.
///
/// This is intentionally in `signature.rs` rather than `std_sigs.rs` to avoid
/// an lsp_types dependency in the feature-independent check crate. It IS called
/// from Task 2.3 (completion detail) and Task 2.4 (hover) via `pub(crate)`.
pub(crate) fn render_sig_label(
    prefix: &str,
    sig: &crate::check::std_sigs::StdSig,
) -> (String, Vec<[u32; 2]>) {
    let mut label = format!("{prefix}(");
    let open_len_u16 = utf16_len(&label);
    let mut offsets: Vec<[u32; 2]> = Vec::new();
    let mut running = open_len_u16;

    for (i, p) in sig.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
            running += 2;
        }
        let param_str = format_std_param(p);
        let start = running;
        running += utf16_len(&param_str);
        let end = running;
        offsets.push([start, end]);
        label.push_str(&param_str);
    }
    label.push(')');

    if let Some(ret) = sig.ret {
        label.push_str(" -> ");
        label.push_str(ret);
    }

    (label, offsets)
}

fn format_std_param(p: &crate::check::std_sigs::StdParam) -> String {
    let mut s = String::new();
    if p.variadic {
        s.push_str("...");
    }
    s.push_str(p.name);
    if let Some(ty) = p.ty {
        s.push_str(": ");
        s.push_str(ty);
    }
    if p.optional && !p.variadic {
        if let Some(def) = p.default {
            s.push_str(" = ");
            s.push_str(def);
        }
    }
    s
}

/// Count UTF-16 code units in a string (BMP chars = 1, supplementary = 2).
fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

// ─────────────────────────────────────────────────────────────────────────────
// Active-param clamping
// ─────────────────────────────────────────────────────────────────────────────

fn clamp_active(active: u32, params: &[ParamInfo]) -> u32 {
    if params.is_empty() {
        return 0;
    }
    // Clamp to the last param index (for variadic, this keeps the cursor on the
    // rest param regardless of how many args have been provided).
    active.min((params.len() - 1) as u32)
}

fn clamp_active_std(active: u32, params: &[crate::check::std_sigs::StdParam]) -> u32 {
    if params.is_empty() {
        return 0;
    }
    // Clamp to last param index — for variadic params this keeps the cursor on
    // the `...rest` parameter regardless of argument count.
    active.min((params.len() - 1) as u32)
}

fn clamp_active_exported(active: u32, params: &[crate::lsp::workspace::ExportedParam]) -> u32 {
    if params.is_empty() {
        return 0;
    }
    active.min((params.len() - 1) as u32)
}

// ─────────────────────────────────────────────────────────────────────────────
// SignatureHelp constructors
// ─────────────────────────────────────────────────────────────────────────────

/// Build a `SignatureHelp` from user-defined params (Simple labels, no docs).
fn make_help_user(name: &str, params: &[ParamInfo], active: u32) -> SignatureHelp {
    let param_labels: Vec<&str> = params.iter().map(|p| p.label.as_str()).collect();
    let label = format!("{name}({})", param_labels.join(", "));
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.label.clone()),
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

/// Build a `SignatureHelp` from stdlib/builtin sig with LabelOffsets + doc.
fn make_help_std(
    label: &str,
    offsets: Vec<[u32; 2]>,
    sig: &crate::check::std_sigs::StdSig,
    active: u32,
) -> SignatureHelp {
    let parameters: Vec<ParameterInformation> = offsets
        .iter()
        .map(|&[s, e]| ParameterInformation {
            label: ParameterLabel::LabelOffsets([s, e]),
            documentation: None,
        })
        .collect();
    let documentation = if sig.doc.is_empty() {
        None
    } else {
        Some(Documentation::String(sig.doc.to_string()))
    };
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: label.to_string(),
            documentation,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    }
}

/// Build a `SignatureHelp` from a cross-file exported fn signature.
fn make_help_exported(
    name: &str,
    sig: &crate::lsp::workspace::ExportedFnSig,
    active: u32,
) -> SignatureHelp {
    let param_labels: Vec<String> = sig.params.iter().map(format_exported_param).collect();
    let label = if let Some(ret) = &sig.ret {
        format!("{name}({}) -> {ret}", param_labels.join(", "))
    } else {
        format!("{name}({})", param_labels.join(", "))
    };
    let parameters: Vec<ParameterInformation> = param_labels
        .iter()
        .map(|l| ParameterInformation {
            label: ParameterLabel::Simple(l.clone()),
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

fn format_exported_param(p: &crate::lsp::workspace::ExportedParam) -> String {
    let mut s = String::new();
    if p.variadic {
        s.push_str("...");
    }
    s.push_str(&p.name);
    // A param with a default value is rendered as `name?: type` (the `?` signals
    // optional; the default value itself is omitted in v1 — callers see the shape,
    // not the concrete default expression).
    if p.optional && !p.variadic {
        s.push('?');
    }
    if let Some(ty) = &p.ty {
        s.push_str(": ");
        s.push_str(ty);
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// CST walk helpers
// ─────────────────────────────────────────────────────────────────────────────

/// The callee + the `ArgList` node of the INNERMOST `CallExpr` whose arg list
/// span contains `offset`.
fn enclosing_call(model: &SemanticModel, offset: usize) -> Option<(Callee, ResolvedNode)> {
    let mut best: Option<(Callee, ResolvedNode, usize)> = None;
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
        // The upper bound for "cursor is inside this call's arguments":
        //  - TERMINATED call (has a closing `)`): the `)` token's start position.
        //    A cursor at or before the `)` is inside; anything past it is not — so
        //    a completed INNER call cannot win over the enclosing call once the
        //    cursor is past the inner `)` (e.g. `pow(abs(x), 2)`: the `2` is in
        //    pow's args, not abs's). This is the nested-call correctness fix.
        //  - UNTERMINATED call (no `)` yet): keep a 2-byte slop past the ArgList
        //    end so a trailing `f(a, <cursor>` (whose space falls outside the
        //    range because no `)` was parsed) still resolves.
        let rparen_start = arg_list
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .filter(|t| t.kind() == SyntaxKind::RParen)
            .last()
            .map(|t| usize::from(t.text_range().start()));
        let upper = rparen_start.unwrap_or(e + 2);
        if offset < s || offset > upper {
            continue;
        }
        let len = e - s;
        // Determine the callee kind.
        let callee_opt = call.children().find(|c| {
            matches!(
                c.kind(),
                SyntaxKind::NameRef | SyntaxKind::MemberExpr
            )
        });
        let Some(callee_node) = callee_opt else {
            continue;
        };
        let callee = match callee_node.kind() {
            SyntaxKind::NameRef => {
                let Some(name) = crate::syntax::resolve::ident_text(callee_node) else {
                    continue;
                };
                Callee::Named(name)
            }
            SyntaxKind::MemberExpr => {
                // Property = last Ident token.
                let property = callee_node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .filter(|t| t.kind() == SyntaxKind::Ident)
                    .last()
                    .map(|t| t.text().to_string())?;
                // Receiver = first expression child (usually a NameRef).
                let recv_node =
                    callee_node.children().find(|c| c.kind() == SyntaxKind::NameRef)?;
                let receiver = crate::syntax::resolve::ident_text(recv_node)?;
                Callee::Member { receiver, property }
            }
            _ => continue,
        };
        if best.as_ref().map(|b| len < b.2).unwrap_or(true) {
            best = Some((callee, arg_list.clone(), len));
        }
    }
    best.map(|(c, a, _)| (c, a))
}

/// The unique in-file `FnDecl` named `name`, if exactly one exists.
fn find_fn_decl(model: &SemanticModel, name: &str) -> Option<ResolvedNode> {
    let mut found = model.tree.descendants().filter(|n| {
        n.kind() == SyntaxKind::FnDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    });
    let first = found.next()?.clone();
    if found.next().is_some() {
        return None; // ambiguous
    }
    Some(first)
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

/// Extract the leading user-CLASS identifier from a rendered `CheckTy` string.
fn first_class_ident(rendered: &str) -> Option<String> {
    const BUILTIN: &[&str] = &[
        "number", "string", "bool", "nil", "any", "array", "map", "future", "bytes", "regex",
        "object", "void", "never", "int", "float", "set",
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

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
        let off = src.rfind("add(").unwrap() + "add(".len();
        let help = signature_help(&m, off, None, None).expect("help");
        assert_eq!(help.signatures[0].label, "add(a, b)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn nested_call_inner_arg_picks_inner_outer_arg_picks_outer() {
        // Regression (SIG §3.1): a completed inner call must NOT win past its own
        // closing `)`. For `math.pow(math.abs(x), 2)` the cursor on pow's SECOND
        // argument (the `2`) must show `math.pow`, not the inner `math.abs`.
        let src = "import * as math from \"std/math\"\nmath.pow(math.abs(x), 2)\n";
        let m = model(src);
        // Cursor inside the INNER abs( arg list → inner signature.
        let off_inner = src.rfind("abs(").unwrap() + "abs(".len();
        let inner = signature_help(&m, off_inner, None, None).expect("inner help");
        assert!(
            inner.signatures[0].label.starts_with("math.abs("),
            "inner arg should resolve abs, got {}",
            inner.signatures[0].label
        );
        // Cursor on pow's second argument (the `2`) → OUTER signature.
        let off_outer = src.rfind('2').unwrap();
        let outer = signature_help(&m, off_outer, None, None).expect("outer help");
        assert!(
            outer.signatures[0].label.starts_with("math.pow("),
            "second arg of pow should resolve pow, got {}",
            outer.signatures[0].label
        );
    }

    #[test]
    fn active_param_advances_past_comma() {
        let src = "fn add(a, b) { return a + b }\nadd(1, 2)\n";
        let m = model(src);
        let off = src.rfind('2').unwrap();
        let help = signature_help(&m, off, None, None).expect("help");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn none_outside_a_call() {
        let m = model("let x = 1\n");
        assert!(signature_help(&m, 4, None, None).is_none());
    }

    #[test]
    fn stdlib_member_call_shows_signature_with_docs() {
        let src = "import * as math from \"std/math\"\nmath.pow(2, \n";
        let m = model(src);
        let off = src.rfind("pow(").unwrap() + "pow(".len();
        let help = signature_help(&m, off, None, None).expect("help");
        let sig = &help.signatures[0];
        assert_eq!(sig.label, "math.pow(base: number, exp: number) -> float");
        assert_eq!(help.active_parameter, Some(0));
        assert!(matches!(&sig.parameters.as_ref().unwrap()[0].label,
            ParameterLabel::LabelOffsets([s, e]) if e > s));
        assert!(format!("{:?}", sig.documentation).contains("Raise a base"));
        let off2 = src.rfind(", ").unwrap() + 2;
        let help2 = signature_help(&m, off2, None, None).expect("help");
        assert_eq!(help2.active_parameter, Some(1));
    }

    #[test]
    fn builtin_call_shows_signature() {
        let src = "print(1)\n";
        let m = model(src);
        let off = src.find("print(").unwrap() + "print(".len();
        let help = signature_help(&m, off, None, None).expect("builtin help");
        assert!(help.signatures[0].label.starts_with("print("));
    }

    #[test]
    fn method_on_typed_receiver_shows_named_params() {
        let src =
            "class C {\n  fn m(count: int, label) { return count }\n}\nlet c = C()\nc.m(\n";
        let m = model(src);
        let off = src.rfind("c.m(").unwrap() + "c.m(".len();
        let help = signature_help(&m, off, None, None).expect("method help");
        assert_eq!(help.signatures[0].label, "m(count: int, label)");
    }

    #[test]
    fn variadic_active_param_clamps_to_rest() {
        let src = "import * as math from \"std/math\"\nmath.min(1, 2, 3\n";
        let m = model(src);
        let off = src.rfind('3').unwrap();
        let help = signature_help(&m, off, None, None).expect("variadic help");
        assert_eq!(help.active_parameter, Some(0), "clamped to ...nums");
    }

    #[test]
    fn unknown_member_returns_none() {
        let src = "import * as math from \"std/math\"\nmath.nosuch(1\n";
        let m = model(src);
        let off = src.rfind("nosuch(").unwrap() + "nosuch(".len();
        assert!(signature_help(&m, off, None, None).is_none());
    }

    #[test]
    fn same_file_fn_still_wins_over_builtin_shadow() {
        let src = "fn print(a) {}\nprint(1)\n";
        let m = model(src);
        let off = src.rfind("print(").unwrap() + "print(".len();
        assert_eq!(
            signature_help(&m, off, None, None).unwrap().signatures[0].label,
            "print(a)"
        );
    }
}
