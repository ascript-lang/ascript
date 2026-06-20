//! `std/markdown` — CommonMark → HTML, **sanitized by default** (BATT T2-3,
//! spec §13).
//!
//! Rendering is pulldown-cmark (`Parser` → `push_html`) with the GFM extensions
//! toggled, then — UNLESS `{sanitize: false}` — the produced HTML is piped
//! through the B4 HTML sanitizer ([`crate::stdlib::html::sanitize_with`]) so any
//! embedded raw HTML / `<script>` / `javascript:` URL comes out inert. The
//! sanitizer is the SINGLE source of truth for the allowlist (this module never
//! forks it); a fenced-code language hint (`<code class="language-x">`) survives
//! because the markdown pipeline permits `class` on `code`/`pre`.
//!
//! `{sanitize: false}` is the documented escape hatch for TRUSTED input only —
//! the docs carry the XSS warning.
//!
//! ## Honest subset
//!
//! CommonMark + GFM tables + strikethrough + task-lists (on by default) +
//! footnotes (off by default). NO front-matter, NO syntax highlighting (only the
//! `class="language-x"` fencing hint is emitted), NO MDX.
//!
//! Pure; **ungated** (no `required_cap`); behind the `markdown` Cargo feature.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{Value, ValueKind};
use pulldown_cmark::{html as cmark_html, Options, Parser};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("render", bi("markdown.render")),
        ("escape", bi("markdown.escape")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "render" => {
            let text = want_string(&arg(args, 0), span, "markdown.render")?;
            render(&text, args.get(1), span)
        }
        "escape" => {
            let s = want_string(&arg(args, 0), span, "markdown.escape")?;
            Ok(Value::str(escape_markdown(&s)))
        }
        _ => Err(AsError::at(format!("std/markdown has no function '{}'", func), span).into()),
    }
}

/// Render CommonMark `text` to HTML, sanitized by default.
///
/// `opts` (slab-mode-safe — every field read goes through the `ObjectCell::get`
/// accessor, NEVER `o.borrow()`):
///   `{ sanitize? = true, gfmTables? = true, strikethrough? = true,
///      taskLists? = true, footnotes? = false, allow? = {...} }`
fn render(text: &str, opts: Option<&Value>, span: Span) -> Result<Value, Control> {
    // Extension defaults (spec §13): tables / strikethrough / task-lists ON,
    // footnotes OFF.
    let mut gfm_tables = true;
    let mut strikethrough = true;
    let mut task_lists = true;
    let mut footnotes = false;
    let mut sanitize = true;
    // The `allow` sub-object (forwarded to the sanitizer) — kept as an owned
    // Value so the borrow on `opts` does not outlive this read.
    let mut allow: Option<Value> = None;

    if let Some(opts) = opts {
        if let ValueKind::Object(o) = opts.kind() {
            // Slab-safe field reads (D1 blocker — NEVER `o.borrow()`).
            if let Some(v) = o.get("sanitize") {
                sanitize = truthy_flag(&v, sanitize);
            }
            if let Some(v) = o.get("gfmTables") {
                gfm_tables = truthy_flag(&v, gfm_tables);
            }
            if let Some(v) = o.get("strikethrough") {
                strikethrough = truthy_flag(&v, strikethrough);
            }
            if let Some(v) = o.get("taskLists") {
                task_lists = truthy_flag(&v, task_lists);
            }
            if let Some(v) = o.get("footnotes") {
                footnotes = truthy_flag(&v, footnotes);
            }
            allow = o.get("allow");
        }
    }

    let mut options = Options::empty();
    if gfm_tables {
        options.insert(Options::ENABLE_TABLES);
    }
    if strikethrough {
        options.insert(Options::ENABLE_STRIKETHROUGH);
    }
    if task_lists {
        options.insert(Options::ENABLE_TASKLISTS);
    }
    if footnotes {
        options.insert(Options::ENABLE_FOOTNOTES);
    }

    let parser = Parser::new_ext(text, options);
    let mut raw_html = String::with_capacity(text.len() + text.len() / 2);
    cmark_html::push_html(&mut raw_html, parser);

    if sanitize {
        let clean = crate::stdlib::html::sanitize_with(&raw_html, allow.as_ref(), span)?;
        Ok(Value::str(clean))
    } else {
        Ok(Value::str(raw_html))
    }
}

/// Read a boolean-ish opts flag. Only an explicit `bool` flips the default; a
/// `nil` (absent-but-present key) leaves the default unchanged; any other value
/// type is treated by truthiness (matching the language's falsy set indirectly —
/// here we accept only `bool`, falling back to the default for non-bool to keep
/// the option matrix predictable).
fn truthy_flag(v: &Value, default: bool) -> bool {
    match v.kind() {
        ValueKind::Bool(b) => b,
        ValueKind::Nil => default,
        _ => default,
    }
}

/// CommonMark ASCII-punctuation metacharacter set that a backslash escape
/// neutralizes (CommonMark §2.4 "Backslash escapes"): any ASCII punctuation
/// character may be backslash-escaped.
fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn render_str(input: &str) -> String {
        let r = call("render", &[Value::str(input)], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("render did not return a string"),
        }
    }

    fn render_opts(input: &str, opts: Value) -> String {
        let r = call("render", &[Value::str(input), opts], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("render did not return a string"),
        }
    }

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::object(m)
    }

    fn esc(input: &str) -> String {
        let r = call("escape", &[Value::str(input)], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!(),
        }
    }

    // ── (a) CommonMark spot vectors ─────────────────────────────────────────

    #[test]
    fn commonmark_headings_emphasis() {
        assert!(render_str("# Hello").contains("<h1>Hello</h1>"));
        assert!(render_str("*em*").contains("<em>em</em>"));
        assert!(render_str("**strong**").contains("<strong>strong</strong>"));
    }

    #[test]
    fn commonmark_links() {
        let out = render_str("[x](https://example.test/p)");
        assert!(out.contains(r#"href="https://example.test/p""#), "got {out:?}");
        assert!(out.contains(">x</a>"), "got {out:?}");
    }

    #[test]
    fn commonmark_lists_blockquotes() {
        let ul = render_str("- a\n- b\n");
        assert!(ul.contains("<ul>") && ul.contains("<li>a</li>"), "got {ul:?}");
        let bq = render_str("> quoted\n");
        assert!(bq.contains("<blockquote>") && bq.contains("quoted"), "got {bq:?}");
    }

    #[test]
    fn fenced_code_info_string_class_survives_sanitize() {
        // pulldown emits <pre><code class="language-rust">; the class MUST survive
        // the default (sanitizing) pipeline.
        let out = render_str("```rust\nfn main() {}\n```\n");
        assert!(out.contains(r#"class="language-rust""#), "class dropped: {out:?}");
        assert!(out.contains("<pre>") && out.contains("<code"), "got {out:?}");
    }

    // ── (b) sanitize-by-default security pins ───────────────────────────────

    #[test]
    fn script_in_markdown_is_inert() {
        // A raw HTML block <script> must come out escaped/dropped, never live.
        let out = render_str("<script>alert(1)</script>\n");
        assert!(
            !out.to_ascii_lowercase().contains("<script"),
            "FAIL-OPEN: live <script: {out:?}"
        );
    }

    #[test]
    fn javascript_url_href_dropped() {
        let out = render_str("[x](javascript:alert(1))");
        assert!(
            !out.to_ascii_lowercase().replace(char::is_whitespace, "").contains("javascript:"),
            "FAIL-OPEN: javascript: href survived: {out:?}"
        );
    }

    #[test]
    fn entity_obfuscated_scheme_neutralized() {
        // An entity-obfuscated scheme through the full markdown→sanitize pipeline.
        let out = render_str("[x](java&#115;cript:alert(1))");
        assert!(
            !out.to_ascii_lowercase().replace(char::is_whitespace, "").contains("javascript:"),
            "FAIL-OPEN: entity-obfuscated scheme survived: {out:?}"
        );
    }

    #[test]
    fn raw_html_table_onclick_stripped() {
        // A raw-HTML block smuggling an onclick must have the handler stripped.
        let out = render_str("<table><tr><td onclick=\"alert(1)\">x</td></tr></table>\n");
        assert!(
            !out.to_ascii_lowercase().contains("onclick"),
            "FAIL-OPEN: onclick survived: {out:?}"
        );
    }

    #[test]
    fn sanitize_false_emits_raw() {
        // The escape hatch: trusted input passes through un-sanitized.
        let out = render_opts(
            "<script>alert(1)</script>\n",
            obj(vec![("sanitize", Value::bool_(false))]),
        );
        assert!(out.contains("<script>"), "raw HTML not emitted: {out:?}");
    }

    // ── (c) extension toggles ───────────────────────────────────────────────

    #[test]
    fn gfm_tables_on_by_default() {
        let out = render_str("| a | b |\n| - | - |\n| 1 | 2 |\n");
        assert!(out.contains("<table>"), "tables off by default: {out:?}");
    }

    #[test]
    fn strikethrough_on_by_default() {
        let out = render_str("~~gone~~");
        assert!(out.contains("<del>gone</del>"), "strikethrough off: {out:?}");
    }

    #[test]
    fn task_lists_on_by_default() {
        let out = render_str("- [x] done\n- [ ] todo\n");
        assert!(out.contains("type=\"checkbox\""), "task-lists off: {out:?}");
    }

    #[test]
    fn footnotes_off_by_default_on_when_enabled() {
        let src = "text[^1]\n\n[^1]: note\n";
        let off = render_str(src);
        assert!(!off.contains("footnote"), "footnotes leaked when off: {off:?}");
        let on = render_opts(src, obj(vec![("footnotes", Value::bool_(true))]));
        assert!(on.contains("footnote"), "footnotes not enabled: {on:?}");
    }

    #[test]
    fn tables_off_when_disabled() {
        let out = render_opts(
            "| a | b |\n| - | - |\n| 1 | 2 |\n",
            obj(vec![("gfmTables", Value::bool_(false))]),
        );
        assert!(!out.contains("<table>"), "tables not disabled: {out:?}");
    }

    // ── (d) allow forwarded to the sanitizer ────────────────────────────────

    #[test]
    fn allow_forwarded_to_sanitizer() {
        // By default a raw <mark> is escaped (not allowlisted). With
        // allow.tags = ["mark", ...] it survives.
        let plain = render_str("<mark>hi</mark>\n");
        assert!(plain.contains("&lt;mark&gt;"), "mark not escaped by default: {plain:?}");

        let allow = obj(vec![(
            "allow",
            obj(vec![(
                "tags",
                Value::array(vec![
                    Value::str("mark"),
                    Value::str("p"),
                ]),
            )]),
        )]);
        let out = render_opts("<mark>hi</mark>\n", allow);
        assert!(out.contains("<mark>hi</mark>"), "allow.tags not forwarded: {out:?}");
    }

    // ── (e) markdown.escape table ───────────────────────────────────────────

    #[test]
    fn escape_table() {
        assert_eq!(esc("a*b_c"), "a\\*b\\_c");
        assert_eq!(esc("# h"), "\\# h");
        assert_eq!(esc("[x](y)"), "\\[x\\]\\(y\\)");
        // Non-punctuation left untouched.
        assert_eq!(esc("plain text 123"), "plain text 123");
        // A backslash is itself punctuation → doubled.
        assert_eq!(esc("a\\b"), "a\\\\b");
    }
}
