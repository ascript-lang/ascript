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

/// Render a `///` Markdown body to HTML (review finding 2 / spec §2: the body is
/// Markdown — code fences, inline code, links, and emphasis must work, not render
/// literally). A small hand-rolled renderer mirroring `docs/assets/app.js`
/// `renderMarkdown`/`renderInline` — NO new crate dependency. Block grammar:
/// fenced ```` ``` ```` code → `<pre><code>` (HTML-escaped contents), blank-line-
/// separated paragraphs → `<p>` (inline-rendered). Inline: `` `code` `` →
/// `<code>`, `[text](url)` → `<a>`, `**bold**` → `<strong>`, `*italic*` → `<em>`.
/// ALL text + code-fence contents are HTML-escaped BEFORE insertion (no XSS — a
/// `<script>` in a body stays inert).
pub fn render_markdown(md: &str) -> String {
    let normalized = md.replace('\r', "");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut html = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Fenced code block: ``` or ```lang … ```.
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            let _lang = rest.trim();
            i += 1;
            let mut buf: Vec<&str> = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                buf.push(lines[i]);
                i += 1;
            }
            // Skip the closing fence (if present).
            if i < lines.len() {
                i += 1;
            }
            html.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&buf.join("\n"))));
            continue;
        }
        // Blank line: paragraph separator.
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        // A paragraph runs until a blank line or a fence.
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len()
            && !lines[i].trim().is_empty()
            && !lines[i].trim_start().starts_with("```")
        {
            para.push(lines[i]);
            i += 1;
        }
        html.push_str(&format!("<p>{}</p>\n", render_inline(&para.join(" "))));
    }
    html
}

/// Render inline Markdown to HTML: inline code (contents protected + escaped),
/// links, bold, italic. Order matters — protect inline-code spans first so their
/// contents are not re-processed, escape the surrounding text, then apply the
/// link/emphasis transforms (whose markup is HTML we intentionally emit), and
/// finally restore the escaped code spans.
fn render_inline(s: &str) -> String {
    // 1. Extract inline-code spans, replacing each with a private-use placeholder.
    let mut codes: Vec<String> = Vec::new();
    let mut protected = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            let mut code = String::new();
            let mut closed = false;
            for d in chars.by_ref() {
                if d == '`' {
                    closed = true;
                    break;
                }
                code.push(d);
            }
            if closed {
                protected.push('\u{E000}');
                protected.push_str(&codes.len().to_string());
                protected.push('\u{E000}');
                codes.push(code);
            } else {
                // An unterminated backtick — keep it literally.
                protected.push('`');
                protected.push_str(&code);
            }
        } else {
            protected.push(c);
        }
    }
    // 2. Escape all remaining text (the placeholders survive — they are PUA chars).
    let mut out = esc(&protected);
    // 3. Links [text](url) — escape both parts; mark external links.
    out = replace_links(&out);
    // 4. Bold then italic.
    out = replace_delimited(&out, "**", "strong");
    out = replace_delimited(&out, "*", "em");
    // 5. Restore inline-code spans (their contents escaped).
    for (idx, code) in codes.iter().enumerate() {
        let placeholder = format!("\u{E000}{idx}\u{E000}");
        out = out.replace(&placeholder, &format!("<code>{}</code>", esc(code)));
    }
    out
}

/// Replace `[text](url)` with `<a href="url">text</a>`. Operates on already-escaped
/// text, so the `[`/`]`/`(`/`)` literals are intact; the url is attribute-escaped.
fn replace_links(s: &str) -> String {
    let mut out = String::new();
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '[' {
            // Find the matching `]` then `(` … `)`.
            if let Some(close) = find_from(&bytes, i + 1, ']') {
                if close + 1 < bytes.len() && bytes[close + 1] == '(' {
                    if let Some(paren) = find_from(&bytes, close + 2, ')') {
                        let text: String = bytes[i + 1..close].iter().collect();
                        let url: String = bytes[close + 2..paren].iter().collect();
                        let external = url.starts_with("http://") || url.starts_with("https://");
                        let ext_attr = if external {
                            " target=\"_blank\" rel=\"noopener\""
                        } else {
                            ""
                        };
                        out.push_str(&format!(
                            "<a href=\"{}\"{}>{}</a>",
                            attr_esc(&url),
                            ext_attr,
                            text
                        ));
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Index of the first `target` at or after `start`, if any.
fn find_from(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|&j| chars[j] == target)
}

/// Replace `<delim>inner<delim>` spans with `<tag>inner</tag>` (non-nesting, the
/// shortest match). `delim` is `**` (bold) or `*` (italic). Already-escaped input,
/// so the delimiters are literal asterisks (`*` is not escaped by `esc`).
fn replace_delimited(s: &str, delim: &str, tag: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find(delim) {
        let after = &rest[start + delim.len()..];
        if let Some(end) = after.find(delim) {
            let inner = &after[..end];
            // A `*` italic must not swallow `**` bold boundaries: require non-empty,
            // non-asterisk-bounded inner for the single-`*` case.
            if delim == "*" && (inner.is_empty() || inner.starts_with('*') || inner.ends_with('*'))
            {
                out.push_str(&rest[..start + delim.len()]);
                rest = after;
                continue;
            }
            out.push_str(&rest[..start]);
            out.push_str(&format!("<{tag}>{inner}</{tag}>"));
            rest = &after[end + delim.len()..];
        } else {
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Attribute-escape a URL (escape `"` and the angle/amp chars).
fn attr_esc(s: &str) -> String {
    esc(s)
}

/// A page filename for a module — its `slug` (the SINGLE source of truth shared
/// with the Markdown emitter and the index link, derived from the root-relative
/// name, so two same-stem files in different dirs get distinct files — finding 1).
pub fn module_filename(module: &DocModule) -> String {
    format!("{}.html", module.slug)
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
        body.push_str(&render_markdown(&doc.body));
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
        body.push_str(&render_markdown(&doc.body));
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
        vec![extract_module(Path::new("calc.as"), "calc", src, &exp, false)]
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
        let m = extract_module(Path::new("e.as"), "e", src, &exp, false);
        let html = render_module(&m);
        // `array<number>` in the signature must be escaped, not raw `<number>`.
        assert!(html.contains("array&lt;number&gt;"), "signature escaped");
    }

    /// Review finding 2: an HTML doc body with a fence + inline code + a link +
    /// bold renders as `<pre>`/`<code>`/`<a>`/`<strong>`, NOT literal characters;
    /// and a `<script>` in a body is still escaped (no XSS).
    #[test]
    fn html_renders_markdown_body() {
        let src = "/// Uses `len(x)` and **bold** text.\n/// See [the guide](https://example.com/g).\n///\n/// ```ascript\n/// let x = 1\n/// ```\nexport fn f() {}\n";
        let exp: HashSet<String> = ["f".to_string()].into_iter().collect();
        let m = extract_module(Path::new("md.as"), "md", src, &exp, false);
        let html = render_module(&m);
        assert!(html.contains("<code>len(x)</code>"), "inline code: {html}");
        assert!(html.contains("<strong>bold</strong>"), "bold: {html}");
        assert!(
            html.contains("<a href=\"https://example.com/g\""),
            "link: {html}"
        );
        assert!(html.contains("<pre><code>let x = 1"), "fence → pre/code: {html}");
        // The literal markdown markers must NOT survive as text.
        assert!(!html.contains("**bold**"), "bold markers consumed: {html}");
        assert!(!html.contains("```"), "fence markers consumed: {html}");
    }

    /// XSS guard: a `<script>` in a doc body is escaped, never emitted raw.
    #[test]
    fn html_escapes_script_in_doc_body() {
        let src = "/// danger <script>alert(1)</script> here\nexport fn f() {}\n";
        let exp: HashSet<String> = ["f".to_string()].into_iter().collect();
        let m = extract_module(Path::new("x.as"), "x", src, &exp, false);
        let html = render_module(&m);
        assert!(!html.contains("<script>"), "no raw script tag: {html}");
        assert!(html.contains("&lt;script&gt;"), "script escaped: {html}");
    }

    /// The render_markdown helper escapes code-fence contents too.
    #[test]
    fn fence_contents_escaped() {
        let html = render_markdown("```\n<b>not bold</b>\n```");
        assert!(html.contains("&lt;b&gt;"), "fence content escaped: {html}");
        assert!(!html.contains("<b>not bold"), "no raw tag: {html}");
    }
}
