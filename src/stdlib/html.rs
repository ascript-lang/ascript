//! `std/html` — HTML escape / unescape + a **fail-closed** allowlist sanitizer.
//!
//! BATT B4 §7.3. HTML is not XML: the sanitizer needs a tokenizer that NEVER
//! rejects input (a strict parser that bailed on malformed markup would leave the
//! raw bytes un-sanitized — a fail-OPEN hole). The security model is therefore
//! **emit-from-parse**:
//!
//! * A lenient tokenizer turns the input into a stream of `text` / `start-tag` /
//!   `end-tag` / `comment` / `doctype` tokens. It cannot error — anything it does
//!   not understand (`<<script>`, an unterminated `<a href="x`) is surfaced as
//!   literal TEXT.
//! * A canonical serializer re-emits ONLY allowlisted elements, with ONLY
//!   allowlisted attributes, every text/attribute value HTML-escaped on emission.
//!   The raw input bytes of a tag are **never echoed** — the output is rebuilt
//!   from scratch, so an attacker cannot smuggle live markup through a parser
//!   quirk.
//! * A non-allowlisted tag is **escaped as text** (`<script>` → `&lt;script&gt;`),
//!   not silently dropped: the user still sees it, but it is inert.
//! * `href`/`src`/`action`/`cite`/… URL attribute values are scheme-checked AFTER
//!   entity-decoding + control/whitespace-stripping + lowercasing, so
//!   `javascript:`, ` javascript:`, `java\tscript:`, `&#106;avascript:`, and
//!   `JaVaScRiPt:` are all neutralized.
//! * Comments, CDATA, processing instructions, doctypes, and the raw content of
//!   `<script>`/`<style>` are stripped — never passed through as live markup.
//!
//! ## Default allowlist (documented verbatim)
//!
//! **Tags:** `p`, `br`, `b`, `strong`, `i`, `em`, `u`, `s`, `code`, `pre`,
//! `blockquote`, `h1`, `h2`, `h3`, `h4`, `h5`, `h6`, `ul`, `ol`, `li`, `a`,
//! `img`, `table`, `thead`, `tbody`, `tr`, `th`, `td`, `hr`, `span`.
//!
//! **Per-tag attributes:** `a` → `href`, `title`; `img` → `src`, `alt`, `title`;
//! every other allowlisted tag has NO attributes (no `class`, no `style`, no
//! `id` — those carry their own injection surface and are opt-in via
//! `opts.attrs`). The global default attribute set is empty.
//!
//! **URL schemes** (for `href`/`src`/`action`/`cite`/`background`/`longdesc`/
//! `poster`/`formaction`/`xlink:href`): `http`, `https`, `mailto` — PLUS relative
//! URLs (no scheme at all, e.g. `/path`, `./x`, `#frag`).
//!
//! `opts.tags` REPLACES the tag allowlist; `opts.attrs` REPLACES the per-tag
//! attribute map; `opts.schemes` REPLACES the scheme allowlist. (Replace, not
//! merge — explicit and predictable.)

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{Value, ValueKind};
use std::collections::HashMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("escape", bi("html.escape")),
        ("unescape", bi("html.unescape")),
        ("sanitize", bi("html.sanitize")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("html.{}", f);
    match func {
        "escape" => {
            let s = want_string(&arg(args, 0), span, &ctx("escape"))?;
            Ok(Value::str(escape(&s)))
        }
        "unescape" => {
            let s = want_string(&arg(args, 0), span, &ctx("unescape"))?;
            // `unescape` is total: an unknown `&name;` is left verbatim (HTML5
            // parsing-error recovery), so there is no error channel. Return a
            // plain string (NOT a Tier-1 pair) — distinct from xml.unescape,
            // which Tier-1-errors on undefined entities by design.
            Ok(Value::str(unescape(&s)))
        }
        "sanitize" => {
            let s = want_string(&arg(args, 0), span, &ctx("sanitize"))?;
            let policy = Policy::from_opts(args.get(1), span)?;
            Ok(Value::str(sanitize(&s, &policy)))
        }
        _ => Err(AsError::at(format!("std/html has no function '{}'", func), span).into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// escape / unescape
// ─────────────────────────────────────────────────────────────────────────────

/// HTML-escape text for safe inclusion in element content OR a double-quoted
/// attribute value. Escapes `& < > " '` (the conservative set — escaping both
/// quote forms means the output is safe in either quoting context).
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A small set of HTML5 named entities (the core that round-trips with `escape`
/// plus a handful of common ones). Unknown names are left verbatim by
/// `unescape`, matching browser parse-error recovery.
fn named_entities() -> &'static HashMap<&'static str, &'static str> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("amp", "&");
        m.insert("lt", "<");
        m.insert("gt", ">");
        m.insert("quot", "\"");
        m.insert("apos", "'");
        m.insert("nbsp", "\u{00A0}");
        m.insert("copy", "\u{00A9}");
        m.insert("reg", "\u{00AE}");
        m.insert("trade", "\u{2122}");
        m.insert("hellip", "\u{2026}");
        m.insert("mdash", "\u{2014}");
        m.insert("ndash", "\u{2013}");
        m.insert("lsquo", "\u{2018}");
        m.insert("rsquo", "\u{2019}");
        m.insert("ldquo", "\u{201C}");
        m.insert("rdquo", "\u{201D}");
        m.insert("middot", "\u{00B7}");
        m.insert("deg", "\u{00B0}");
        m.insert("euro", "\u{20AC}");
        m.insert("pound", "\u{00A3}");
        m.insert("cent", "\u{00A2}");
        m.insert("yen", "\u{00A5}");
        m.insert("sect", "\u{00A7}");
        m.insert("para", "\u{00B6}");
        m
    })
}

/// Decode HTML named + numeric entities. Total: an unrecognized `&name;` or a
/// malformed numeric ref is emitted verbatim (no error).
pub(crate) fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Copy the next char (handles multibyte by char boundary).
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // Find the terminating ';' within a bounded window (entities are short).
        let rest = &s[i + 1..];
        let semi = rest[..rest.len().min(32)].find(';');
        let Some(semi) = semi else {
            out.push('&');
            i += 1;
            continue;
        };
        let body = &rest[..semi];
        if let Some(decoded) = decode_entity_body(body) {
            out.push_str(&decoded);
            i += 1 + semi + 1; // '&' + body + ';'
        } else {
            out.push('&');
            i += 1;
        }
    }
    out
}

/// Decode the body of an entity (between `&` and `;`). Returns `None` if not a
/// recognized entity.
fn decode_entity_body(body: &str) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    if let Some(num) = body.strip_prefix('#') {
        // Numeric character reference.
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        let ch = char::from_u32(code)?;
        return Some(ch.to_string());
    }
    named_entities().get(body).map(|s| s.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// sanitizer policy
// ─────────────────────────────────────────────────────────────────────────────

/// URL-bearing attribute names (always scheme-checked when allowlisted).
const URL_ATTRS: &[&str] = &[
    "href",
    "src",
    "action",
    "formaction",
    "cite",
    "background",
    "longdesc",
    "poster",
    "xlink:href",
];

/// Default allowlisted tags (§7.3).
const DEFAULT_TAGS: &[&str] = &[
    "p", "br", "b", "strong", "i", "em", "u", "s", "code", "pre", "blockquote", "h1", "h2", "h3",
    "h4", "h5", "h6", "ul", "ol", "li", "a", "img", "table", "thead", "tbody", "tr", "th", "td",
    "hr", "span",
];

/// Void (self-closing, no end tag) elements.
const VOID_TAGS: &[&str] = &["br", "img", "hr", "input"];

/// Raw-text elements whose content is text (not markup) and which we DROP
/// entirely (never echoing script/style payloads).
const RAW_TEXT_TAGS: &[&str] = &["script", "style", "noscript", "iframe", "object", "embed", "svg",
    "math", "template", "title", "textarea"];

#[derive(Clone)]
struct Policy {
    /// Allowed tag → its allowed attribute set.
    tags: HashMap<String, Vec<String>>,
    /// Allowed URL schemes (lowercased, no trailing colon).
    schemes: Vec<String>,
}

impl Policy {
    fn default_policy() -> Self {
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        for t in DEFAULT_TAGS {
            tags.insert((*t).to_string(), Vec::new());
        }
        tags.insert("a".to_string(), vec!["href".to_string(), "title".to_string()]);
        tags.insert(
            "img".to_string(),
            vec!["src".to_string(), "alt".to_string(), "title".to_string()],
        );
        Policy {
            tags,
            schemes: vec!["http".to_string(), "https".to_string(), "mailto".to_string()],
        }
    }

    fn from_opts(opts: Option<&Value>, span: Span) -> Result<Policy, Control> {
        let policy = Policy::default_policy();
        let Some(opts) = opts else { return Ok(policy) };
        let ValueKind::Object(o) = opts.kind() else {
            // A non-object opts (e.g. nil) → defaults.
            return Ok(policy);
        };
        Policy::apply_object(policy, o, span)
    }

    /// Build a policy by applying a `{tags, attrs, schemes}` object onto a base
    /// policy. This is the SINGLE source of truth for policy construction —
    /// `html.sanitize` and `std/markdown` (via [`sanitize_with`]) both reach it,
    /// so the allowlist semantics can never diverge between the two paths.
    ///
    /// `o` is a slab-mode-safe `ObjectCell` view — every field read goes through
    /// the `ObjectCell::get`/`entries` accessors (NEVER `o.borrow()`, which panics
    /// on a slab-mode source-literal object).
    fn apply_object(
        mut policy: Policy,
        o: &crate::value::ObjectCell,
        span: Span,
    ) -> Result<Policy, Control> {
        // opts.tags: array<string> → REPLACES the tag set (each gets the default
        // attrs for that tag if a default exists, else no attrs).
        if let Some(tags_v) = o.get("tags") {
            if let ValueKind::Array(a) = tags_v.kind() {
                let defaults = Policy::default_policy();
                let mut tags: HashMap<String, Vec<String>> = HashMap::new();
                for v in a.borrow().iter() {
                    let name = want_string(v, span, "html.sanitize opts.tags")?
                        .to_ascii_lowercase();
                    let attrs = defaults.tags.get(&name).cloned().unwrap_or_default();
                    tags.insert(name, attrs);
                }
                policy.tags = tags;
            }
        }

        // opts.attrs: object<string, array<string>> → REPLACES the per-tag attr
        // map. A tag mentioned here must already be in the tag allowlist to take
        // effect (otherwise the attrs are moot — the tag is escaped anyway).
        if let Some(attrs_v) = o.get("attrs") {
            if let ValueKind::Object(am) = attrs_v.kind() {
                for (tag, list) in am.entries() {
                    let tag = tag.to_ascii_lowercase();
                    let mut allowed = Vec::new();
                    if let ValueKind::Array(la) = list.kind() {
                        for v in la.borrow().iter() {
                            let an = want_string(v, span, "html.sanitize opts.attrs")?
                                .to_ascii_lowercase();
                            allowed.push(an);
                        }
                    }
                    // Only meaningful for an allowlisted tag.
                    if policy.tags.contains_key(&tag) {
                        policy.tags.insert(tag, allowed);
                    }
                }
            }
        }

        // opts.schemes: array<string> → REPLACES the scheme set.
        if let Some(schemes_v) = o.get("schemes") {
            if let ValueKind::Array(a) = schemes_v.kind() {
                let mut schemes = Vec::new();
                for v in a.borrow().iter() {
                    let s = want_string(v, span, "html.sanitize opts.schemes")?
                        .to_ascii_lowercase();
                    schemes.push(s.trim_end_matches(':').to_string());
                }
                policy.schemes = schemes;
            }
        }

        Ok(policy)
    }

    /// Permit `class` on any `code`/`pre` tag that is currently allowlisted.
    /// `std/markdown` calls this so a fenced-code language hint
    /// (`<code class="language-rust">`, emitted by pulldown-cmark) survives the
    /// sanitize pass. A `class` value carries no executable surface (it is plain
    /// text, HTML-escaped on emission), so this is safe to add by default for the
    /// markdown pipeline. Tags not in the allowlist are left untouched.
    fn allow_code_class(&mut self) {
        for tag in ["code", "pre"] {
            if let Some(attrs) = self.tags.get_mut(tag) {
                if !attrs.iter().any(|a| a == "class") {
                    attrs.push("class".to_string());
                }
            }
        }
    }

    /// Permit the tags pulldown-cmark generates for the enabled GFM extensions
    /// that are NOT in the default `html.sanitize` allowlist, so legitimate
    /// rendered output survives the markdown pipeline:
    ///   * `del` — strikethrough (`~~x~~`),
    ///   * `input` (with `type`/`checked`/`disabled` only) — the task-list
    ///     checkbox, which pulldown emits as `<input disabled type="checkbox">`.
    ///
    /// None of these carry an executable surface: the task-list `input` is always
    /// `disabled` and value-less, and every attribute value is HTML-escaped on
    /// emission. A tag the user has ALREADY removed via `allow.tags` is left
    /// removed (we only ADD when not present — same posture as `allow_code_class`),
    /// EXCEPT we always (re)add when missing so the extension output is not
    /// silently corrupted; callers who do not want these can post-process. (In
    /// practice the extension is what produced the tag, so allowing it is correct.)
    fn allow_markdown_generated(&mut self) {
        self.tags.entry("del".to_string()).or_default();
        // `input` gets a fixed, inert attribute set (NOT user-overridable here —
        // these are the only attrs the task-list renderer emits).
        let input_attrs = self.tags.entry("input".to_string()).or_default();
        for a in ["type", "checked", "disabled"] {
            if !input_attrs.iter().any(|x| x == a) {
                input_attrs.push(a.to_string());
            }
        }
    }

    fn tag_allowed(&self, tag: &str) -> bool {
        self.tags.contains_key(tag)
    }

    fn attr_allowed(&self, tag: &str, attr: &str) -> bool {
        self.tags.get(tag).map(|v| v.iter().any(|a| a == attr)).unwrap_or(false)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// lenient tokenizer
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Token {
    /// Literal text (raw, NOT yet escaped — the serializer escapes on emit).
    Text(String),
    /// A start tag: name (lowercased) + (attr-name, attr-value) pairs.
    StartTag {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    /// An end tag: name (lowercased).
    EndTag(String),
    /// A comment / doctype / PI / CDATA — always dropped.
    Ignore,
}

/// Tokenize leniently. NEVER errors. Anything that does not form a well-shaped
/// tag becomes `Text` (the fail-closed core — a stray `<` is literal text).
fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut text_start = 0;

    macro_rules! flush_text {
        ($end:expr) => {{
            if $end > text_start {
                tokens.push(Token::Text(chars[text_start..$end].iter().collect()));
            }
        }};
    }

    while i < n {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        // A '<' not followed by a tag-name char / '/' / '!' is literal text
        // (the `<<script>` classic: the first `<` is text).
        let next = chars.get(i + 1).copied();
        match next {
            Some('!') => {
                // Comment `<!-- ... -->`, CDATA `<![CDATA[...]]>`, or doctype
                // `<!DOCTYPE ...>`. All dropped. Find the end leniently.
                flush_text!(i);
                let (consumed, _) = consume_bang(&chars, i);
                tokens.push(Token::Ignore);
                i = consumed;
                text_start = i;
            }
            Some('?') => {
                // Processing instruction — drop to the next '>'.
                flush_text!(i);
                let mut j = i + 2;
                while j < n && chars[j] != '>' {
                    j += 1;
                }
                if j < n {
                    j += 1;
                }
                tokens.push(Token::Ignore);
                i = j;
                text_start = i;
            }
            Some('/') => {
                // End tag `</name>`.
                if let Some((tok, end)) = parse_end_tag(&chars, i) {
                    flush_text!(i);
                    tokens.push(tok);
                    i = end;
                    text_start = i;
                } else {
                    // Malformed — `<` is literal text.
                    i += 1;
                }
            }
            Some(c) if c.is_ascii_alphabetic() => {
                // Start tag.
                if let Some((tok, end)) = parse_start_tag(&chars, i) {
                    flush_text!(i);
                    tokens.push(tok);
                    i = end;
                    text_start = i;
                } else {
                    // Unterminated tag (`<script` at EOF, `<a href="x`) — the
                    // rest is literal text (fail-closed: escaped on emit).
                    i += 1;
                }
            }
            _ => {
                // `<<`, `< `, `<` at EOF, `<=` … — the `<` is literal text.
                i += 1;
            }
        }
    }
    flush_text!(n);
    tokens
}

/// Consume a `<!...>` construct. Handles `<!-- ... -->` (to `-->`),
/// `<![CDATA[ ... ]]>` (to `]]>`), and `<!DOCTYPE ...>` / generic `<! ... >`
/// (to `>`). Returns the index just past the construct.
fn consume_bang(chars: &[char], start: usize) -> (usize, ()) {
    let n = chars.len();
    // Comment?
    if chars.get(start + 2) == Some(&'-') && chars.get(start + 3) == Some(&'-') {
        let mut j = start + 4;
        while j + 2 < n {
            if chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>' {
                return (j + 3, ());
            }
            j += 1;
        }
        return (n, ());
    }
    // CDATA?
    let cdata: Vec<char> = "[CDATA[".chars().collect();
    if chars[start + 2..].starts_with(&cdata[..]) {
        let mut j = start + 2 + cdata.len();
        while j + 2 < n {
            if chars[j] == ']' && chars[j + 1] == ']' && chars[j + 2] == '>' {
                return (j + 3, ());
            }
            j += 1;
        }
        return (n, ());
    }
    // Generic `<! ... >` (doctype etc.) — to the next '>'.
    let mut j = start + 2;
    while j < n && chars[j] != '>' {
        j += 1;
    }
    if j < n {
        j += 1;
    }
    (j, ())
}

/// Parse `</name>` starting at `start` (chars[start]=='<', chars[start+1]=='/').
/// Returns the token + index just past '>'. `None` if no terminating '>'.
fn parse_end_tag(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let n = chars.len();
    let mut j = start + 2;
    let name_start = j;
    while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == ':') {
        j += 1;
    }
    if j == name_start {
        return None;
    }
    let name: String = chars[name_start..j].iter().collect::<String>().to_ascii_lowercase();
    // Skip to '>'.
    while j < n && chars[j] != '>' {
        j += 1;
    }
    if j >= n {
        return None;
    }
    Some((Token::EndTag(name), j + 1))
}

/// Parse a start tag `<name attr=val ...>` (possibly self-closing `/>`).
/// Returns the token + the index just past '>'. `None` if there is no
/// terminating '>' (unterminated → caller treats `<` as literal text).
fn parse_start_tag(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let n = chars.len();
    let mut j = start + 1;
    let name_start = j;
    while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == ':') {
        j += 1;
    }
    let name: String = chars[name_start..j].iter().collect::<String>().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }

    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut self_closing = false;

    loop {
        // Skip whitespace.
        while j < n && chars[j].is_whitespace() {
            j += 1;
        }
        if j >= n {
            // Unterminated tag — fail closed (caller escapes the raw `<`).
            return None;
        }
        if chars[j] == '>' {
            return Some((
                Token::StartTag {
                    name,
                    attrs,
                    self_closing,
                },
                j + 1,
            ));
        }
        if chars[j] == '/' {
            // `/>` self-closing (or a stray '/').
            if chars.get(j + 1) == Some(&'>') {
                self_closing = true;
                return Some((
                    Token::StartTag {
                        name,
                        attrs,
                        self_closing,
                    },
                    j + 2,
                ));
            }
            j += 1;
            continue;
        }
        // Attribute name.
        let an_start = j;
        while j < n
            && !chars[j].is_whitespace()
            && chars[j] != '='
            && chars[j] != '>'
            && chars[j] != '/'
        {
            j += 1;
        }
        if j == an_start {
            // Defensive: a char we did not consume (e.g. a lone '=') — skip it.
            j += 1;
            continue;
        }
        let attr_name: String =
            chars[an_start..j].iter().collect::<String>().to_ascii_lowercase();

        // Optional `= value`.
        while j < n && chars[j].is_whitespace() {
            j += 1;
        }
        let mut attr_val = String::new();
        if j < n && chars[j] == '=' {
            j += 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j >= n {
                return None; // unterminated
            }
            match chars[j] {
                '"' | '\'' => {
                    let quote = chars[j];
                    j += 1;
                    let v_start = j;
                    while j < n && chars[j] != quote {
                        j += 1;
                    }
                    if j >= n {
                        // Unterminated quoted value → fail closed: treat the
                        // whole tag as literal text.
                        return None;
                    }
                    attr_val = chars[v_start..j].iter().collect();
                    j += 1; // past closing quote
                }
                _ => {
                    // UNQUOTED value — the classic bypass. Read to whitespace or
                    // '>'. This correctly SPLITS `href=https://x onclick=...`
                    // into two attributes, so the event handler is then dropped.
                    let v_start = j;
                    while j < n && !chars[j].is_whitespace() && chars[j] != '>' {
                        j += 1;
                    }
                    attr_val = chars[v_start..j].iter().collect();
                }
            }
        }
        attrs.push((attr_name, attr_val));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// sanitize (emit-from-parse serializer)
// ─────────────────────────────────────────────────────────────────────────────

/// Sanitize `input` under `policy`. The output contains only allowlisted,
/// canonically-serialized markup; everything else is escaped as inert text.
fn sanitize(input: &str, policy: &Policy) -> String {
    let tokens = tokenize(input);
    let mut out = String::with_capacity(input.len());
    // Stack of currently-open ALLOWLISTED, non-void elements (for auto-close).
    let mut open: Vec<String> = Vec::new();
    // When inside a dropped raw-text element, swallow its content entirely.
    let mut skip_until: Option<String> = None;

    for tok in tokens {
        // Raw-text drop region: swallow everything until the matching end tag.
        if let Some(ref skip) = skip_until {
            if let Token::EndTag(name) = &tok {
                if name == skip {
                    skip_until = None;
                }
            }
            continue;
        }

        match tok {
            Token::Ignore => { /* comments/doctype/CDATA/PI dropped */ }
            Token::Text(t) => {
                out.push_str(&escape(&t));
            }
            Token::EndTag(name) => {
                if policy.tag_allowed(&name) && !VOID_TAGS.contains(&name.as_str()) {
                    // Close down to (and including) the matching open tag, if any.
                    if let Some(pos) = open.iter().rposition(|t| *t == name) {
                        while open.len() > pos {
                            let t = open.pop().unwrap();
                            out.push_str("</");
                            out.push_str(&t);
                            out.push('>');
                        }
                    }
                    // else: stray end tag with no opener → drop silently.
                }
                // Non-allowlisted end tag → drop (the start tag was escaped as
                // text, so emitting `&lt;/script&gt;` for the close is harmless
                // but noisy; we drop the close to keep output clean).
            }
            Token::StartTag {
                name,
                attrs,
                self_closing,
            } => {
                if RAW_TEXT_TAGS.contains(&name.as_str()) && !self_closing {
                    // Enter a content-drop region (script/style/iframe/…). Its
                    // raw payload is never emitted.
                    skip_until = Some(name);
                    continue;
                }
                if !policy.tag_allowed(&name) {
                    // Fail-closed: escape the entire tag as inert text. We
                    // reconstruct a canonical `<...>` from the parsed pieces and
                    // escape it (so the user sees the markup, but it is text).
                    out.push_str(&escape(&render_raw_tag(&name, &attrs, self_closing)));
                    continue;
                }

                // Allowlisted: emit a canonical tag with only allowed attrs.
                out.push('<');
                out.push_str(&name);
                for (an, av) in &attrs {
                    if !policy.attr_allowed(&name, an) {
                        continue;
                    }
                    // Drop event-handler attributes defensively even if somehow
                    // allowlisted (belt-and-suspenders).
                    if an.starts_with("on") {
                        continue;
                    }
                    if URL_ATTRS.contains(&an.as_str()) {
                        match sanitize_url(av, policy) {
                            Some(clean) => {
                                out.push(' ');
                                out.push_str(an);
                                out.push_str("=\"");
                                out.push_str(&escape(&clean));
                                out.push('"');
                            }
                            None => { /* unsafe scheme → drop the attribute */ }
                        }
                    } else {
                        out.push(' ');
                        out.push_str(an);
                        out.push_str("=\"");
                        out.push_str(&escape(av));
                        out.push('"');
                    }
                }
                let is_void = VOID_TAGS.contains(&name.as_str());
                if is_void {
                    out.push_str(" />");
                } else if self_closing {
                    // A self-closed non-void allowlisted tag — emit empty.
                    out.push_str("></");
                    out.push_str(&name);
                    out.push('>');
                } else {
                    out.push('>');
                    open.push(name);
                }
            }
        }
    }

    // Auto-close any still-open allowlisted elements (balanced output).
    while let Some(t) = open.pop() {
        out.push_str("</");
        out.push_str(&t);
        out.push('>');
    }
    out
}

/// Rust-side sanitizer entry for `std/markdown` (BATT D3 §13). Sanitize `input`
/// under the DEFAULT html.sanitize policy, optionally narrowed/widened by an
/// `allow` object (`{tags, attrs, schemes}` — the SAME shape `html.sanitize`'s
/// own opts use, applied through the single [`Policy::apply_object`] source of
/// truth so the two sanitize paths can never diverge).
///
/// In addition, `class` is permitted on `code`/`pre` so a fenced-code language
/// hint (`<code class="language-x">`) survives the pass.
///
/// `allow` is read slab-safely (it is a `ValueKind::Object` view → the
/// `ObjectCell::get` accessors); a non-object `allow` (e.g. `nil`) is ignored.
pub(crate) fn sanitize_with(input: &str, allow: Option<&Value>, span: Span) -> Result<String, Control> {
    let mut policy = Policy::default_policy();
    if let Some(allow) = allow {
        if let ValueKind::Object(o) = allow.kind() {
            policy = Policy::apply_object(policy, o, span)?;
        }
    }
    policy.allow_code_class();
    policy.allow_markdown_generated();
    Ok(sanitize(input, &policy))
}

/// Reconstruct the textual form of a non-allowlisted tag so it can be ESCAPED
/// (shown as inert text, never echoed live).
fn render_raw_tag(name: &str, attrs: &[(String, String)], self_closing: bool) -> String {
    let mut s = String::from("<");
    s.push_str(name);
    for (an, av) in attrs {
        s.push(' ');
        s.push_str(an);
        if !av.is_empty() {
            s.push_str("=\"");
            s.push_str(av);
            s.push('"');
        }
    }
    if self_closing {
        s.push_str(" /");
    }
    s.push('>');
    s
}

/// Scheme-check a URL attribute value. Returns the cleaned value if its scheme
/// is allowlisted (or it is relative), else `None` (drop the attribute).
///
/// Defense pipeline: entity-DECODE → strip leading control/whitespace → extract
/// scheme → lowercase → check against the allowlist. This catches
/// `javascript:`, ` javascript:`, `java\tscript:`, `java\nscript:`,
/// `&#106;avascript:`, `JaVaScRiPt:`, and `&colon;`-obfuscated schemes.
fn sanitize_url(raw: &str, policy: &Policy) -> Option<String> {
    // 1. Decode entities first (so `&#106;avascript:` → `javascript:`,
    //    `jav&#x09;ascript:` → `jav\tascript:`, `&colon;` → `:`).
    let decoded = unescape_url(raw);

    // 2. Determine the scheme. The scheme is the run of chars BEFORE the first
    //    ':' — but ONLY if that run, after stripping leading control/whitespace,
    //    is a valid scheme token (alpha then alnum/+/-/.) with no '/', '?', '#'
    //    before the ':'. Otherwise there is no scheme → it's relative.
    let stripped: String = decoded
        .chars()
        .skip_while(|c| is_url_control(*c))
        .collect();

    // Find a ':' that is not preceded by a path/query/fragment separator.
    let mut scheme = String::new();
    let mut has_scheme = false;
    for c in stripped.chars() {
        if c == ':' {
            has_scheme = true;
            break;
        }
        if c == '/' || c == '?' || c == '#' || c == '\\' {
            // Separator before any ':' → relative URL, no scheme.
            break;
        }
        // Strip embedded control/whitespace chars within the scheme region
        // (`java\tscript:` → `javascript:`).
        if is_url_control(c) {
            continue;
        }
        scheme.push(c);
    }

    if !has_scheme || scheme.is_empty() {
        // Relative URL (no scheme) — allowed. Return the decoded-then-stripped
        // value but re-strip leading controls and any embedded control chars
        // that could re-form a scheme on the consumer side.
        let safe: String = decoded.chars().filter(|c| !is_url_control(*c)).collect();
        // A value that, after control-stripping, gained a scheme must be
        // re-checked (paranoia): re-run once with the stripped value.
        if safe != decoded && safe.contains(':') {
            return sanitize_url(&safe, policy);
        }
        return Some(decoded);
    }

    let scheme_lc = scheme.to_ascii_lowercase();
    // Hard deny the dangerous trio regardless of allowlist mistakes.
    if matches!(scheme_lc.as_str(), "javascript" | "vbscript" | "data") {
        return None;
    }
    if policy.schemes.contains(&scheme_lc) {
        Some(decoded)
    } else {
        None
    }
}

/// Control / whitespace chars that browsers strip from URLs before scheme
/// matching: everything in 0x00..=0x20 (incl. tab, newline, CR, NUL, space).
fn is_url_control(c: char) -> bool {
    (c as u32) <= 0x20
}

/// Entity-decode a URL value, additionally honoring `&colon;` (which the
/// general `unescape` does not list) so scheme obfuscation via `&colon;` is
/// caught.
fn unescape_url(raw: &str) -> String {
    let pre = raw.replace("&colon;", ":").replace("&COLON;", ":");
    unescape(&pre)
}

#[cfg(all(test, feature = "xml"))]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn san(input: &str) -> String {
        let r = call("sanitize", &[Value::str(input)], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("sanitize did not return a string"),
        }
    }

    fn san_opts(input: &str, opts: Value) -> String {
        let r = call("sanitize", &[Value::str(input), opts], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("sanitize did not return a string"),
        }
    }

    fn esc(input: &str) -> String {
        let r = call("escape", &[Value::str(input)], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!(),
        }
    }

    fn unesc(input: &str) -> String {
        let r = call("unescape", &[Value::str(input)], sp()).unwrap();
        match r.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!(),
        }
    }

    // ── THE security test: the XSS battery ──────────────────────────────────
    //
    // Each vector's sanitized output MUST satisfy the fail-closed invariants:
    //   - NO `<script` (any case) as live markup,
    //   - NO `on*=` event-handler attribute,
    //   - NO `javascript:` / `data:` / `vbscript:` URL in any attribute,
    //   - a non-allowlisted tag appears only ESCAPED (`&lt;...`), never live.

    /// Assert the universal neutralization invariants on a sanitized output.
    fn assert_neutralized(input: &str, out: &str) {
        let lc = out.to_ascii_lowercase();
        // No live <script (the escaped form is `&lt;script`, which is fine).
        // After lowercasing, a literal `<script` substring means live markup.
        assert!(
            !lc.contains("<script"),
            "FAIL-OPEN: live <script in output for {input:?} -> {out:?}"
        );
        assert!(
            !lc.contains("<iframe") && !lc.contains("<object") && !lc.contains("<embed")
                && !lc.contains("<svg") && !lc.contains("<math"),
            "FAIL-OPEN: live dangerous element in output for {input:?} -> {out:?}"
        );
        // No event-handler attribute. We look for ` on...=` patterns in live
        // markup. Since allowlisted attrs are emitted as `name="..."`, any
        // occurrence of `on<word>=` outside an escaped/text region is a leak.
        // Conservatively: the output must not contain `onerror`, `onload`,
        // `onclick`, `onmouseover` as part of an unescaped attribute. Because
        // we escape `<`/`>` in text, any surviving handler would be in a live
        // tag. Check there is no ` on...="` or ` on...=` attribute form.
        for handler in ["onerror", "onload", "onclick", "onmouseover"] {
            // It's fine if it appears inside escaped text (preceded by &lt; not <).
            // A simple robust check: it must not appear immediately following a
            // space inside a live tag. We approximate: the dangerous form is
            // `<tag ... onX=`. Since live tags are only allowlisted ones with
            // only allowlisted attrs, a handler name cannot appear in a live tag
            // at all — assert its absence as an attribute token.
            assert!(
                !live_attr_present(out, handler),
                "FAIL-OPEN: event handler {handler} live in output for {input:?} -> {out:?}"
            );
        }
        // No dangerous URL scheme in a live attribute value.
        for bad in ["javascript:", "vbscript:", "data:text/html"] {
            assert!(
                !lc.replace(char::is_whitespace, "").contains(bad),
                "FAIL-OPEN: dangerous scheme {bad} in output for {input:?} -> {out:?}"
            );
        }
    }

    /// Is `attr` present as a live (unescaped) attribute name in `out`?
    /// Live markup uses `<`; escaped text uses `&lt;`. We scan for the handler
    /// only within `<...>` live-tag spans.
    fn live_attr_present(out: &str, attr: &str) -> bool {
        let bytes = out.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                // Find the end of this live tag.
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'>' {
                    j += 1;
                }
                let span = out[i..j.min(out.len())].to_ascii_lowercase();
                if span.contains(attr) {
                    return true;
                }
                i = j;
            } else {
                i += 1;
            }
        }
        false
    }

    #[test]
    fn sanitizer_xss_battery() {
        // (vector, optional positive substring that MUST survive)
        let vectors: &[&str] = &[
            // script element, case variants
            "<script>alert(1)</script>",
            "<ScRiPt>alert(1)</ScRiPt>",
            "<SCRIPT >alert(1)</SCRIPT>",
            // event-handler attributes
            "<img src=x onerror=alert(1)>",
            "<img src=x OnErRoR=alert(1)>",
            // javascript: URLs, case + whitespace + tab + newline + entity
            r#"<a href="javascript:alert(1)">x</a>"#,
            r#"<a href="JaVaScRiPt:alert(1)">x</a>"#,
            r#"<a href=" javascript:alert(1)">x</a>"#,
            "<a href=\"java\tscript:alert(1)\">x</a>",
            "<a href=\"java\nscript:alert(1)\">x</a>",
            r#"<a href="&#106;avascript:alert(1)">x</a>"#,
            r#"<a href="jav&#x09;ascript:alert(1)">x</a>"#,
            r#"<a href="javascript&colon;alert(1)">x</a>"#,
            // dangerous elements + attributes
            "<svg onload=alert(1)>",
            r#"<math href="javascript:alert(1)">"#,
            "<iframe src=evil></iframe>",
            "<object data=evil></object>",
            "<embed src=evil>",
            // mXSS
            r#"<noscript><p title="</noscript><img src=x onerror=alert(1)>">"#,
            "<style>@import 'evil'</style>",
            "<!-- <script>alert(1)</script> -->",
            "<![CDATA[<script>alert(1)</script>]]>",
            // malformed
            "<<script>alert(1)</script>",
            "<script",
            r#"<a href="x"#,
            "<a href=https://x onclick=alert(1)>x</a>",
            r#"<a href="x" "onmouseover=alert(1)">x</a>"#,
            // data: URL
            r#"<img src="data:text/html,<script>alert(1)</script>">"#,
        ];
        for v in vectors {
            let out = san(v);
            assert_neutralized(v, &out);
        }
        // Sanity: we tested the documented count of vectors.
        assert_eq!(vectors.len(), 28, "vector count drift");
    }

    #[test]
    fn positive_cases_survive() {
        // Allowlisted formatting re-emitted clean.
        assert_eq!(san("<b>bold</b>"), "<b>bold</b>");
        assert_eq!(san("<i>x</i> <em>y</em>"), "<i>x</i> <em>y</em>");
        assert_eq!(san("<p>para</p>"), "<p>para</p>");
        assert_eq!(
            san(r#"<a href="https://x.test/p">link</a>"#),
            r#"<a href="https://x.test/p">link</a>"#
        );
        // Relative href survives.
        assert_eq!(san(r#"<a href="/rel">r</a>"#), r#"<a href="/rel">r</a>"#);
        assert_eq!(san(r##"<a href="#frag">r</a>"##), r##"<a href="#frag">r</a>"##);
        // mailto survives.
        assert_eq!(
            san(r#"<a href="mailto:x@y.test">m</a>"#),
            r#"<a href="mailto:x@y.test">m</a>"#
        );
        // List structure.
        assert_eq!(san("<ul><li>a</li><li>b</li></ul>"), "<ul><li>a</li><li>b</li></ul>");
        // img with allowed attrs.
        let out = san(r#"<img src="/a.png" alt="A">"#);
        assert!(out.contains("src=\"/a.png\""));
        assert!(out.contains("alt=\"A\""));
    }

    #[test]
    fn non_allowlisted_tag_is_escaped_not_dropped() {
        // A non-allowlisted tag must appear ESCAPED (visible but inert), per the
        // fail-closed-shows-as-text rule.
        let out = san("<unknown>hi</unknown>");
        assert!(out.contains("&lt;unknown&gt;"), "got {out:?}");
        assert!(!out.contains("<unknown>"));
        // And its text content survives (escaped tag + plain text).
        assert!(out.contains("hi"));
    }

    #[test]
    fn unquoted_attr_bypass_split() {
        // `href=https://x onclick=alert(1)` must split: href kept, onclick dropped.
        let out = san("<a href=https://x onclick=alert(1)>x</a>");
        assert!(out.contains("href=\"https://x\""), "got {out:?}");
        assert!(!out.to_ascii_lowercase().contains("onclick"), "got {out:?}");
    }

    #[test]
    fn custom_options_honored() {
        use indexmap::IndexMap;
        // Custom tags: allow only <mark>.
        let mut opts = IndexMap::new();
        opts.insert("tags".to_string(), Value::array(vec![Value::str("mark")]));
        let out = san_opts("<mark>m</mark><b>b</b>", Value::object(opts));
        assert!(out.contains("<mark>m</mark>"), "got {out:?}");
        // <b> is no longer allowlisted → escaped.
        assert!(out.contains("&lt;b&gt;"), "got {out:?}");

        // Custom schemes: allow `ftp`.
        let mut opts2 = IndexMap::new();
        opts2.insert(
            "schemes".to_string(),
            Value::array(vec![Value::str("ftp"), Value::str("https")]),
        );
        let out2 = san_opts(r#"<a href="ftp://h/f">f</a>"#, Value::object(opts2));
        assert!(out2.contains("href=\"ftp://h/f\""), "got {out2:?}");

        // Custom attrs: allow `class` on span.
        let mut attrs = IndexMap::new();
        attrs.insert(
            "span".to_string(),
            Value::array(vec![Value::str("class")]),
        );
        let mut opts3 = IndexMap::new();
        opts3.insert("attrs".to_string(), Value::object(attrs));
        let out3 = san_opts(r#"<span class="hl">x</span>"#, Value::object(opts3));
        assert!(out3.contains("class=\"hl\""), "got {out3:?}");
    }

    #[test]
    fn escape_table() {
        assert_eq!(esc("a & b < c > d \" e ' f"), "a &amp; b &lt; c &gt; d &quot; e &#39; f");
    }

    #[test]
    fn unescape_table() {
        assert_eq!(unesc("a &amp; b &lt; c &gt; d &quot; e &#39; f"), "a & b < c > d \" e ' f");
        assert_eq!(unesc("&copy;&nbsp;x"), "\u{00A9}\u{00A0}x");
        assert_eq!(unesc("&#65;&#x42;"), "AB");
        // Unknown entity left verbatim.
        assert_eq!(unesc("&notreal;"), "&notreal;");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        for s in ["plain", "a<b>c&d\"e'f", "<script>", "100% & more", "tabs\tand\nnewlines"] {
            let round = unesc(&esc(s));
            assert_eq!(&round, s, "roundtrip failed for {s:?}");
        }
    }

    #[test]
    fn comments_doctype_stripped() {
        assert_eq!(san("<!-- hi --><p>x</p>"), "<p>x</p>");
        assert_eq!(san("<!DOCTYPE html><p>x</p>"), "<p>x</p>");
        assert_eq!(san("<![CDATA[raw]]><p>x</p>"), "<p>x</p>");
    }

    #[test]
    fn unbalanced_tags_autoclosed() {
        // Unclosed allowlisted tags are auto-closed for balanced output.
        let out = san("<b><i>x");
        assert_eq!(out, "<b><i>x</i></b>");
    }

    #[test]
    fn void_elements() {
        assert_eq!(san("<br>"), "<br />");
        assert_eq!(san("<hr>after"), "<hr />after");
    }
}
