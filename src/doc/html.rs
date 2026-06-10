//! HTML emitter for `ascript doc --format html` (DX D1, spec §3.4).
//!
//! Emits a SELF-CONTAINED tree (default `target/doc/`): one `index.html` plus one
//! page per module, sharing one embedded `style.css`. The look mirrors the
//! `docs/` site's warm "ink & amber" palette, but the tree is **independent** —
//! it carries its OWN index (no dependency on the `docs/assets/app.js` `NAV`
//! array, so a generated page can never be orphaned). Folding generated docs into
//! the hand-written `docs/` site would be an explicit opt requiring a `NAV` edit;
//! this default never does.

use crate::doc::{DocItem, DocModule};

/// The self-contained stylesheet, mirroring the `docs/` site palette. Embedded so
/// the tree needs no external asset path.
pub const STYLE_CSS: &str = r#":root{--ink:#14110d;--panel:#1b1712;--card:#211c16;--line:#2a2419;--cream:#efe6d6;--muted:#9d917b;--amber:#f0a92b;--sky:#74b6c9;}
*{box-sizing:border-box}
body{margin:0;background:var(--ink);color:var(--cream);font-family:'IBM Plex Sans',system-ui,sans-serif;line-height:1.55}
.wrap{max-width:880px;margin:0 auto;padding:2.5rem 1.5rem}
h1,h2,h3{font-weight:600;color:var(--cream)}
h1{font-size:1.9rem;border-bottom:1px solid var(--line);padding-bottom:.4rem}
h2{margin-top:2rem;color:var(--amber)}
h3{margin-top:1.4rem;color:var(--sky)}
a{color:var(--amber);text-decoration:none}
a:hover{text-decoration:underline}
code,pre{font-family:'IBM Plex Mono',ui-monospace,monospace}
pre{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.8rem 1rem;overflow-x:auto}
code{background:var(--panel);padding:.1rem .35rem;border-radius:4px;font-size:.9em}
pre code{background:none;padding:0}
ul{padding-left:1.2rem}
.kind{color:var(--muted);font-size:.85em;font-weight:400}
.priv{color:var(--muted);font-style:italic;font-size:.8em}
.index li{margin:.3rem 0}
.summary{color:var(--muted)}
nav.crumbs{margin-bottom:1.5rem;font-size:.9em;color:var(--muted)}
"#;

/// HTML-escape `&`, `<`, `>`, `"` for safe text/attribute interpolation.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A page filename for a module (its name + `.html`).
pub fn module_filename(module: &DocModule) -> String {
    format!("{}.html", sanitize(&module.name))
}

/// Sanitize a module name into a filesystem-safe slug.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// The shared HTML document shell.
fn page(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n<div class=\"wrap\">\n{}\n</div>\n</body>\n</html>\n",
        esc(title),
        body
    )
}

/// Render the index page linking every module.
pub fn render_index(modules: &[DocModule]) -> String {
    let mut body = String::from("<h1>API documentation</h1>\n<ul class=\"index\">\n");
    for m in modules {
        let summary = m
            .module_doc
            .as_ref()
            .map(|d| d.summary.clone())
            .unwrap_or_default();
        body.push_str(&format!(
            "<li><a href=\"{}\">{}</a> <span class=\"summary\">{}</span></li>\n",
            esc(&module_filename(m)),
            esc(&m.name),
            esc(&summary)
        ));
    }
    body.push_str("</ul>\n");
    page("API documentation", &body)
}

/// Render one module's page.
pub fn render_module(module: &DocModule) -> String {
    let mut body = String::new();
    body.push_str("<nav class=\"crumbs\"><a href=\"index.html\">index</a> / ");
    body.push_str(&esc(&module.name));
    body.push_str("</nav>\n");
    body.push_str(&format!("<h1>{}</h1>\n", esc(&module.name)));

    if let Some(doc) = &module.module_doc {
        body.push_str(&format!("<p>{}</p>\n", esc(&doc.body)));
    }

    if module.items.is_empty() {
        body.push_str("<p class=\"summary\">No public API.</p>\n");
        return page(&module.name, &body);
    }

    for item in &module.items {
        render_item(&mut body, item, 2);
    }
    page(&module.name, &body)
}

/// Render one item into `body` at heading level `level`.
fn render_item(body: &mut String, item: &DocItem, level: usize) {
    let priv_tag = if item.exported {
        String::new()
    } else {
        " <span class=\"priv\">(private)</span>".to_string()
    };
    body.push_str(&format!(
        "<h{level} id=\"{}\"><span class=\"kind\">{}</span> {}{}</h{level}>\n",
        esc(&item.name),
        esc(item.kind.label()),
        esc(&item.name),
        priv_tag
    ));
    body.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&item.signature)));

    if let Some(doc) = &item.doc {
        body.push_str(&format!("<p>{}</p>\n", esc(&doc.body)));
    }

    if !item.members.is_empty() {
        let heading = match item.kind {
            crate::doc::ItemKind::Enum => "Variants",
            _ => "Fields",
        };
        body.push_str(&format!("<h{} >{heading}</h{}>\n", level + 1, level + 1));
        body.push_str("<ul>\n");
        for m in &item.members {
            let summary = m
                .doc
                .as_ref()
                .filter(|d| !d.summary.is_empty())
                .map(|d| format!(" — {}", esc(&d.summary)))
                .unwrap_or_default();
            body.push_str(&format!(
                "<li><code>{}</code>{}</li>\n",
                esc(&m.signature),
                summary
            ));
        }
        body.push_str("</ul>\n");
    }

    if !item.methods.is_empty() {
        body.push_str(&format!("<h{} >Methods</h{}>\n", level + 1, level + 1));
        for me in &item.methods {
            render_item(body, me, level + 2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::extract_module;
    use std::collections::HashSet;
    use std::path::Path;

    fn mods() -> Vec<DocModule> {
        let src = "//! A calculator.\n/// Adds two numbers.\nexport fn add(a, b) { return a + b }\n";
        let exp: HashSet<String> = ["add".to_string()].into_iter().collect();
        vec![extract_module(Path::new("calc.as"), src, &exp, false)]
    }

    #[test]
    fn index_links_every_module() {
        let ms = mods();
        let html = render_index(&ms);
        assert!(html.contains("<title>API documentation</title>"));
        assert!(html.contains("href=\"calc.html\""));
        assert!(html.contains("A calculator."));
        assert!(html.contains("<link rel=\"stylesheet\" href=\"style.css\">"));
    }

    #[test]
    fn module_page_has_item_and_signature() {
        let ms = mods();
        let html = render_module(&ms[0]);
        assert!(html.contains("<h1>calc</h1>"));
        assert!(html.contains("fn add(a, b)"));
        assert!(html.contains("Adds two numbers."));
        assert!(html.contains("href=\"index.html\""));
    }

    #[test]
    fn html_is_escaped() {
        let src = "/// less &lt; than\nexport fn f(): array<number> { return [] }\n";
        let exp: HashSet<String> = ["f".to_string()].into_iter().collect();
        let m = extract_module(Path::new("e.as"), src, &exp, false);
        let html = render_module(&m);
        // `array<number>` in the signature must be escaped, not raw `<number>`.
        assert!(html.contains("array&lt;number&gt;"), "signature escaped");
    }
}
