//! Markdown emitter for `ascript doc --format md` (DX D1, spec §3.4).
//!
//! Plain `.md` per module, readable straight from the repo. Renders each item's
//! signature in a fenced `ascript` block followed by its `///` doc body, with
//! classes/enums expanding their members.

use crate::doc::{DocItem, DocMember, DocModule};

/// Render a whole module to a Markdown document.
pub fn render_module(module: &DocModule) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", module.name));

    if let Some(doc) = &module.module_doc {
        out.push_str(&doc.body);
        out.push_str("\n\n");
    }

    if module.items.is_empty() {
        out.push_str("_No public API._\n");
        return out;
    }

    for item in &module.items {
        render_item(&mut out, item, 2);
    }
    out
}

/// Render one item at the given heading level (`##` = 2).
fn render_item(out: &mut String, item: &DocItem, level: usize) {
    let hashes = "#".repeat(level);
    let vis = if item.exported { "" } else { " _(private)_" };
    out.push_str(&format!("{hashes} {} `{}`{vis}\n\n", item.kind.label(), item.name));
    out.push_str("```ascript\n");
    out.push_str(&item.signature);
    out.push_str("\n```\n\n");

    if let Some(doc) = &item.doc {
        out.push_str(&doc.body);
        out.push_str("\n\n");
    }

    if !item.members.is_empty() {
        let heading = match item.kind {
            crate::doc::ItemKind::Enum => "Variants",
            _ => "Fields",
        };
        out.push_str(&format!("{} {heading}\n\n", "#".repeat(level + 1)));
        for m in &item.members {
            render_member(out, m);
        }
        out.push('\n');
    }

    if !item.methods.is_empty() {
        out.push_str(&format!("{} Methods\n\n", "#".repeat(level + 1)));
        for me in &item.methods {
            render_item(out, me, level + 2);
        }
    }
}

/// Render a class field / enum variant as a Markdown list entry.
fn render_member(out: &mut String, m: &DocMember) {
    out.push_str(&format!("- `{}`", m.signature));
    if let Some(doc) = &m.doc {
        if !doc.summary.is_empty() {
            out.push_str(" — ");
            out.push_str(&doc.summary);
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::extract_module;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn renders_function_with_signature_and_doc() {
        let src = "/// Adds two numbers.\nexport fn add(a: number, b: number): number { return a + b }\n";
        let exp: HashSet<String> = ["add".to_string()].into_iter().collect();
        let m = extract_module(Path::new("calc.as"), src, &exp, false);
        let md = render_module(&m);
        assert!(md.contains("# calc"));
        assert!(md.contains("## fn `add`"));
        assert!(md.contains("fn add(a: number, b: number): number"));
        assert!(md.contains("Adds two numbers."));
    }

    #[test]
    fn renders_enum_variants() {
        let src = "/// A shape.\nexport enum Shape {\n  /// a circle\n  Circle(r: float),\n  Point,\n}\n";
        let exp: HashSet<String> = ["Shape".to_string()].into_iter().collect();
        let m = extract_module(Path::new("shape.as"), src, &exp, false);
        let md = render_module(&m);
        assert!(md.contains("### Variants"));
        assert!(md.contains("Circle(r: float)"));
        assert!(md.contains("a circle"));
    }
}
