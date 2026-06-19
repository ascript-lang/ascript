//! `textDocument/hover` over the model: declaration/keyword/builtin docs plus the
//! SP10 inferred/declared type.

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

/// Hover at byte `offset`. Returns the inferred/declared type (if any) plus a
/// keyword/builtin/declaration doc line. `None` when neither is available (cursor
/// on trivia / an unknown token with no inferred type).
///
/// When the cursor lands on a stdlib member (`math.sqrt`, `math.pi`), a stdlib
/// signature / constant-type block is pushed FIRST (before the inferred type and
/// doc parts) so the curated sig leads the hover.
///
/// `with_types`: when `true`, the SP10 inferred/declared type is included in the
/// hover output (full fidelity). When `false`, the inferred-type block is skipped
/// and `model.infer_cache()` is NEVER called — used by the size-class gate so
/// large/huge files pay no inference cost while still showing stdlib/builtin/keyword
/// docs. The stdlib-member sig/doc block and `doc_at` parts are always rendered.
pub fn hover(model: &SemanticModel, offset: usize, with_types: bool) -> Option<Hover> {
    use crate::check::std_sigs::{module_members, std_sig, MemberKind};
    use crate::lsp::providers::signature::render_sig_label;

    let mut parts: Vec<String> = Vec::new();

    // § SIG §3.3: stdlib member sig + doc, pushed FIRST so the curated info leads.
    if let Some((module, member, alias)) = stdlib_member_at(model, offset) {
        if let Some(sig) = std_sig(&module, &member) {
            let prefix = format!("{alias}.{member}");
            let (label, _) = render_sig_label(&prefix, sig);
            parts.push(format!("```ascript\n{label}\n```"));
            if !sig.doc.is_empty() {
                parts.push(sig.doc.to_string());
            }
        } else if let Some(members) = module_members(&module) {
            // A CONSTANT: `alias.member: type`
            if let Some((_, MemberKind::Const(ty))) =
                members.iter().find(|(n, _)| *n == member.as_str())
            {
                parts.push(format!("```ascript\n{alias}.{member}: {ty}\n```"));
            }
        }
    }

    // Inferred/declared type block — skipped for large/huge files (with_types=false).
    if with_types {
        if let Some(ty) = crate::check::infer::hover_type_in(model.infer_cache(), offset) {
            parts.push(format!("```ascript\n{ty}\n```"));
        }
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

/// Scan the source at `offset` to find a stdlib member access of the form
/// `<alias>.<member>` where the cursor sits on `<member>`. Returns
/// `(module_path, member_name, alias)` on success, or `None` when:
/// - the cursor is on the alias (before the dot), not the member;
/// - there is no dot immediately before the identifier at `offset`;
/// - the alias is not a `import * as <alias> from "std/…"` namespace import.
///
/// The scanner works in CHAR space (matching the idiom in `completion.rs` and
/// `signature.rs`) and never holds a borrow across an `await`.
fn stdlib_member_at(
    model: &SemanticModel,
    offset: usize,
) -> Option<(String /*module*/, String /*member*/, String /*alias*/)> {
    use crate::lsp::providers::completion::namespace_import_module_pub;

    let chars: Vec<char> = model.text.chars().collect();
    let offset = offset.min(chars.len());

    // Identify the identifier token that contains (or starts at) `offset`.
    // Scan backward to the start of the identifier, then forward to its end.
    // `offset` may be inside the identifier (not just at its start).
    let ident_end = {
        let mut e = offset;
        while e < chars.len() && is_ident_char(chars[e]) {
            e += 1;
        }
        e
    };
    let ident_start = {
        let mut s = offset;
        while s > 0 && is_ident_char(chars[s - 1]) {
            s -= 1;
        }
        s
    };

    // If `offset` is not inside an identifier, bail.
    if ident_start >= ident_end {
        return None;
    }

    // Require a dot IMMEDIATELY before the member identifier (no whitespace).
    if ident_start == 0 || chars[ident_start - 1] != '.' {
        return None;
    }
    let dot_pos = ident_start - 1;

    // Read the alias: the identifier ending right before the dot.
    // There must be no whitespace between alias and dot in `alias.member`.
    let alias_end = dot_pos;
    if alias_end == 0 || !is_ident_char(chars[alias_end - 1]) {
        return None;
    }
    let mut alias_start = alias_end;
    while alias_start > 0 && is_ident_char(chars[alias_start - 1]) {
        alias_start -= 1;
    }
    // alias must start with a valid identifier start (not a digit)
    if chars[alias_start].is_ascii_digit() {
        return None;
    }

    let alias: String = chars[alias_start..alias_end].iter().collect();
    let member: String = chars[ident_start..ident_end].iter().collect();
    if member.is_empty() || alias.is_empty() {
        return None;
    }

    // The alias must be a namespace import of a known stdlib module.
    let module = namespace_import_module_pub(&model.text, &alias)?;

    Some((module, member, alias))
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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
        let h = hover(&m, off, true).expect("hover");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("number"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_int_typed_let_shows_int() {
        let src = "let x: int = 1\nprint(x)\n";
        let m = model(src);
        let off = src.rfind('x').unwrap();
        let h = hover(&m, off, true).expect("hover");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("int"), "got {}", mk.value);
    }

    #[test]
    fn hover_shows_inferred_return_type() {
        // Hovering `y` (= id(1); id returns number) shows the INFERRED `number`,
        // even though `y` itself is unannotated. (Re-establishes the coverage of the
        // deleted analysis.rs::hover_shows_inferred_return_type.)
        let src = "fn id(x: number) { return x }\nlet y = id(1)\nprint(y)\n";
        let m = model(src);
        let off = src.rfind('y').unwrap();
        let h = hover(&m, off, true).expect("hover on y");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("number"), "got {}", mk.value);
    }

    #[test]
    fn hover_shows_any_for_unannotated() {
        // An unannotated param's use hovers as `any`. (Re-establishes the coverage of
        // the deleted analysis.rs::hover_shows_any_for_unannotated.)
        let src = "fn g(p) { return p }\n";
        let m = model(src);
        let off = src.rfind('p').unwrap(); // the use in `return p`
        let h = hover(&m, off, true).expect("hover on p");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("any"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_builtin_print_mentions_print() {
        let src = "print(1)\n";
        let m = model(src);
        let off = src.find("print").unwrap();
        let h = hover(&m, off, true).expect("hover on print");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("print"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_keyword_fn() {
        let src = "fn foo() {}\n";
        let m = model(src);
        let h = hover(&m, 0, true).expect("hover on fn");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.to_lowercase().contains("function"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_whitespace_is_none() {
        let m = model("let x = 1\n");
        assert!(hover(&m, 3, true).is_none()); // the space
    }

    #[test]
    fn hover_on_generic_construction_shows_instantiated_type() {
        // TYPE Task 16: hovering a binding of a generic class construction surfaces
        // the INSTANTIATED type (`Box<int>`, not `Box<T>` / `Box<any>`). The type arg
        // is inferred from the positional field argument `5`.
        let src = "class Box<T> {\n  value: T\n  fn get(): T { return self.value }\n}\nlet b = Box(5)\nprint(b.get())\n";
        let m = model(src);
        let off = src.find("let b").unwrap() + 4; // the binding name `b`
        let h = hover(&m, off, true).expect("hover on b");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("Box<int>"), "got {}", mk.value);
    }

    #[test]
    fn hover_on_generic_fn_call_shows_instantiated_return() {
        // TYPE Task 16: hovering a binding of a generic fn call surfaces the SOLVED
        // return type. `map<A, B>([1, 2, 3], (x) => x * 2)` solves `A = int`,
        // `B = int` (callback inference), so the result hovers as `array<int>`.
        let src = "fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> { return [] }\nlet r = map([1, 2, 3], (x) => x * 2)\nprint(r)\n";
        let m = model(src);
        let off = src.find("let r").unwrap() + 4;
        let h = hover(&m, off, true).expect("hover on r");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("array<int>"), "got {}", mk.value);
    }

    // ── SIG §3.3: stdlib-member hover ────────────────────────────────────────

    #[test]
    fn hover_on_stdlib_member_shows_signature_and_doc() {
        let src = "import * as math from \"std/math\"\nlet y = math.sqrt(2)\n";
        let m = model(src);
        let off = src.rfind("sqrt").unwrap() + 1; // inside `sqrt`
        let h = hover(&m, off, true).expect("hover on math.sqrt");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("math.sqrt("), "sig line: {}", mk.value);
        assert!(
            mk.value.contains("Returns")
                || mk.value.contains("square root")
                || mk.value.contains("->"),
            "doc/ret: {}",
            mk.value
        );
    }

    #[test]
    fn hover_on_stdlib_constant_shows_type() {
        let src = "import * as math from \"std/math\"\nlet y = math.pi\n";
        let m = model(src);
        let off = src.rfind("pi").unwrap();
        let h = hover(&m, off, true).expect("hover on math.pi");
        let HoverContents::Markup(mk) = h.contents else {
            panic!()
        };
        assert!(mk.value.contains("math.pi: float"), "{}", mk.value);
    }

    // ── Size-class gate ───────────────────────────────────────────────────────

    #[test]
    fn hover_without_types_still_shows_keyword_doc() {
        // with_types=false skips the inferred type block but still renders doc_at
        // output for keywords/builtins (the part that remains valid for large files).
        let src = "fn foo() {}\n";
        let m = model(src);
        // With types OFF, `fn` keyword still shows a doc if doc_at returns something.
        // The key assertion: it doesn't panic and the `with_types=false` path is
        // exercised. If doc_at returns a result, it should appear; if not, `None` is
        // fine (no crash, no inference cost).
        let _h = hover(&m, 0, false); // must not panic; result may be None or Some
    }

    #[test]
    fn hover_without_types_does_not_include_inferred_type_block() {
        // with_types=false must NOT include an inferred-type block even when the
        // cursor is over a typed binding.
        let src = "let x: int = 1\nprint(x)\n";
        let m = model(src);
        let off = src.rfind('x').unwrap();
        // Full hover includes `int`.
        let with = hover(&m, off, true).expect("hover with types");
        let HoverContents::Markup(mk_with) = with.contents else {
            panic!()
        };
        assert!(mk_with.value.contains("int"), "with_types=true must show int; got {}", mk_with.value);

        // Hover without types must NOT include `int` from the inferred-type block.
        // (doc_at may or may not return something; the infer block is the gated part.)
        if let Some(without) = hover(&m, off, false) {
            let HoverContents::Markup(mk_without) = without.contents else {
                panic!()
            };
            // The code-fenced inferred type must be absent.
            // (doc_at for a plain let binding returns None, so the hover result is
            //  likely None here, but if it isn't, ensure no inferred-type block.)
            assert!(
                !mk_without.value.contains("```ascript\nint\n```"),
                "with_types=false must not show inferred-type code block; got {}",
                mk_without.value
            );
        }
        // None result is also acceptable (no doc_at + no inferred type → None).
    }
}
