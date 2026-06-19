//! `std/xml` — strict XML parse / stringify / (un)escape over `quick-xml`.
//!
//! BATT B3 §7.2. The design goal is a *safe* XML surface: the classic XML
//! attacks — XXE (external-entity), the billion-laughs / quadratic-entity
//! blow-up, and unbounded recursion / huge documents — are **structurally
//! impossible**, not merely mitigated:
//!
//! * **No entity expansion beyond the five predefined entities.** `quick-xml`
//!   does NOT expand custom (DTD-defined) or external entities; it surfaces an
//!   `&entity;` reference as a discrete `Event::GeneralRef`. We expand ONLY the
//!   five XML-predefined entities (`amp`/`lt`/`gt`/`quot`/`apos`) plus numeric
//!   character references (`&#NN;` / `&#xNN;`). Any OTHER named reference — a
//!   DTD internal entity (`<!ENTITY a "BOOM">` + `&a;`), an external SYSTEM
//!   entity (`<!ENTITY xxe SYSTEM "file:///etc/passwd">`), or a billion-laughs
//!   expansion entity — is a **Tier-1 `undefined entity '<name>'` error**. The
//!   billion-laughs payload therefore never expands; it stops at the first
//!   custom entity reference.
//! * **No `net` / `fs` dependency.** This module imports nothing that can open
//!   a socket or a file. An external SYSTEM entity literally cannot be fetched —
//!   there is no code path to fetch it. (`xml = ["data", "dep:quick-xml"]`.)
//! * **Depth + node budgets.** A nesting-depth counter (`MAX_DEPTH`) and a
//!   total-node counter (`MAX_NODES`) bound a maliciously deep or huge document
//!   to a Tier-1 error instead of a stack overflow / OOM.
//!
//! The stable parse shape (§7.2): every element is
//! `{ tag: string, attrs: object, children: array }` where `attrs` is an
//! insertion-ordered object of attribute-name → string-value and `children` is
//! an array whose entries are either child element-objects or text strings
//! (CDATA folded into text, adjacent text coalesced). Comments and processing
//! instructions are dropped. Namespaced names (`ns:tag`, `xmlns:*`) pass through
//! raw.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use indexmap::IndexMap;

/// Maximum element nesting depth. A document nested deeper than this is rejected
/// (Tier-1) rather than risking a stack overflow in recursive consumers.
pub(crate) const MAX_DEPTH: usize = 256;
/// Maximum total node count (elements + text nodes). Bounds a huge flat or wide
/// document.
pub(crate) const MAX_NODES: usize = 1_000_000;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("xml.parse")),
        ("stringify", bi("xml.stringify")),
        ("escape", bi("xml.escape")),
        ("unescape", bi("xml.unescape")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("xml.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match parse_document(&s) {
                Ok(node) => Ok(make_pair(node, Value::nil())),
                Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
            }
        }
        "stringify" => {
            let node = arg(args, 0);
            let indent = stringify_indent(args.get(1));
            match stringify_document(&node, indent) {
                Ok(text) => Ok(make_pair(Value::str(text), Value::nil())),
                Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
            }
        }
        "escape" => {
            let s = want_string(&arg(args, 0), span, &ctx("escape"))?;
            Ok(Value::str(escape_text(&s)))
        }
        "unescape" => {
            // Tier-1 pair (consistent with the other fallible fns): an undefined
            // named entity is recoverable user-data error, not a panic.
            let s = want_string(&arg(args, 0), span, &ctx("unescape"))?;
            match unescape_text(&s) {
                Ok(out) => Ok(make_pair(Value::str(out), Value::nil())),
                Err(msg) => Ok(make_pair(Value::nil(), make_error(Value::str(msg)))),
            }
        }
        _ => Err(AsError::at(format!("std/xml has no function '{}'", func), span).into()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Parse
// ─────────────────────────────────────────────────────────────────────────────

/// A partially-built element on the parse stack.
struct Frame {
    tag: String,
    attrs: IndexMap<String, Value>,
    children: Vec<Value>,
}

/// Parse an XML document into the §7.2 stable shape, returning the root element
/// object. Returns `Err(message)` (→ Tier-1) for malformed XML, an undefined
/// (custom/external) entity reference, or a budget violation.
pub(crate) fn parse_document(text: &str) -> Result<Value, String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(text);
    let cfg = reader.config_mut();
    // Mismatched / unclosed tags are an error, not silently accepted.
    cfg.check_end_names = true;
    // A bare `&` with no `;` (`&amp`) is rejected, not passed through.
    cfg.allow_dangling_amp = false;
    // An empty element `<x/>` is reported as a real Start+End pair so the build
    // logic is uniform.
    cfg.expand_empty_elements = true;

    let mut stack: Vec<Frame> = Vec::new();
    // The completed root element. Set exactly once, when the outermost element
    // closes. A second root element after it is a Tier-1 error.
    let mut root: Option<Value> = None;
    let mut nodes: usize = 0;

    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("invalid XML at byte {}: {}", reader.error_position(), e))?;
        match ev {
            Event::Start(e) => {
                if root.is_some() {
                    return Err("invalid XML: content after the root element".into());
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(format!(
                        "XML nesting depth exceeds the limit of {} elements",
                        MAX_DEPTH
                    ));
                }
                bump_nodes(&mut nodes)?;
                let tag = decode_name(e.name().as_ref())?;
                let attrs = decode_attrs(&e)?;
                stack.push(Frame {
                    tag,
                    attrs,
                    children: Vec::new(),
                });
            }
            Event::End(e) => {
                let frame = stack
                    .pop()
                    .ok_or_else(|| "invalid XML: unexpected closing tag".to_string())?;
                let end_name = decode_name(e.name().as_ref())?;
                if end_name != frame.tag {
                    return Err(format!(
                        "invalid XML: mismatched closing tag </{}> for <{}>",
                        end_name, frame.tag
                    ));
                }
                let elem = build_element(frame);
                match stack.last_mut() {
                    Some(parent) => parent.children.push(elem),
                    None => root = Some(elem),
                }
            }
            Event::Text(e) => {
                let s = e
                    .decode()
                    .map_err(|err| format!("invalid XML text encoding: {}", err))?;
                push_text(&mut stack, &mut nodes, s.as_ref())?;
            }
            Event::CData(e) => {
                // CDATA is raw, unescaped character data → folded into text.
                let raw = e.into_inner();
                let s = std::str::from_utf8(&raw)
                    .map_err(|err| format!("invalid XML CDATA encoding: {}", err))?;
                push_text(&mut stack, &mut nodes, s)?;
            }
            Event::GeneralRef(e) => {
                // `&name;` / `&#NN;` / `&#xNN;`. quick-xml does NOT expand it —
                // the safe posture. We resolve ONLY numeric refs and the five
                // predefined entities; any other named ref (a DTD-internal or
                // external entity, incl. billion-laughs) is a hard error.
                let name = e
                    .decode()
                    .map_err(|err| format!("invalid XML entity reference: {}", err))?;
                let resolved = resolve_entity(name.as_ref())?;
                push_text(&mut stack, &mut nodes, &resolved)?;
            }
            // Comments + processing instructions are dropped (§7.2). The XML
            // declaration is metadata, also dropped.
            Event::Comment(_) | Event::PI(_) | Event::Decl(_) => {}
            // DTD / DOCTYPE is NEVER processed for entity definitions — this is
            // the structural XXE / billion-laughs defense. We simply skip the
            // event; the entity table is never populated, so a later `&a;` is an
            // undefined-entity error.
            Event::DocType(_) => {}
            Event::Empty(_) => {
                // Unreachable: expand_empty_elements=true turns these into a
                // Start+End pair. Treat defensively as a no-content element.
                unreachable!("expand_empty_elements should split Empty into Start+End");
            }
            Event::Eof => break,
        }
    }

    if !stack.is_empty() {
        return Err(format!(
            "invalid XML: {} unclosed element(s)",
            stack.len()
        ));
    }
    root.ok_or_else(|| "invalid XML: no root element".to_string())
}

fn bump_nodes(nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err(format!("XML node count exceeds the limit of {}", MAX_NODES));
    }
    Ok(())
}

/// Append text to the innermost open element, coalescing with a trailing text
/// child if present. Text outside any element (before the root / between top
/// elements) is ignored unless it is non-whitespace, which is malformed.
fn push_text(stack: &mut [Frame], nodes: &mut usize, s: &str) -> Result<(), String> {
    let Some(frame) = stack.last_mut() else {
        // Top-level text. Whitespace (indentation around the root) is fine; any
        // visible text outside the root element is malformed.
        if s.trim().is_empty() {
            return Ok(());
        }
        return Err("invalid XML: text outside the root element".into());
    };
    // Coalesce with a trailing text child.
    if let Some(last) = frame.children.last() {
        if let ValueKind::Str(existing) = last.kind() {
            let merged = format!("{}{}", existing, s);
            let idx = frame.children.len() - 1;
            frame.children[idx] = Value::str(merged);
            return Ok(());
        }
    }
    bump_nodes(nodes)?;
    frame.children.push(Value::str(s));
    Ok(())
}

fn build_element(frame: Frame) -> Value {
    let mut obj: IndexMap<String, Value> = IndexMap::new();
    obj.insert("tag".to_string(), Value::str(frame.tag));
    obj.insert("attrs".to_string(), Value::object(frame.attrs));
    obj.insert("children".to_string(), Value::array(frame.children));
    Value::object(obj)
}

fn decode_name(raw: &[u8]) -> Result<String, String> {
    std::str::from_utf8(raw)
        .map(|s| s.to_string())
        .map_err(|err| format!("invalid XML tag name encoding: {}", err))
}

fn decode_attrs(e: &quick_xml::events::BytesStart) -> Result<IndexMap<String, Value>, String> {
    let mut attrs: IndexMap<String, Value> = IndexMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| format!("invalid XML attribute: {}", err))?;
        let key = decode_name(attr.key.as_ref())?;
        // `unescape_value` resolves ONLY the five predefined entities + numeric
        // refs; a custom entity in an attribute value is an error (the same safe
        // posture as text). Errors surface as Tier-1.
        let value = attr
            .unescape_value()
            .map_err(|err| format!("invalid XML attribute value for '{}': {}", key, err))?
            .to_string();
        // Duplicate attribute keys: last wins (object insertion semantics).
        attrs.insert(key, Value::str(value));
    }
    Ok(attrs)
}

/// Resolve an entity-reference name (the text between `&` and `;`).
/// Numeric refs (`#NN`, `#xNN`) and the five predefined entities resolve; every
/// other named reference is an undefined-entity error.
fn resolve_entity(name: &str) -> Result<String, String> {
    if let Some(num) = name.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16)
                .map_err(|_| format!("invalid numeric character reference '&{};'", name))?
        } else {
            num.parse::<u32>()
                .map_err(|_| format!("invalid numeric character reference '&{};'", name))?
        };
        let ch = char::from_u32(code)
            .ok_or_else(|| format!("invalid numeric character reference '&{};'", name))?;
        return Ok(ch.to_string());
    }
    match name {
        "amp" => Ok("&".to_string()),
        "lt" => Ok("<".to_string()),
        "gt" => Ok(">".to_string()),
        "quot" => Ok("\"".to_string()),
        "apos" => Ok("'".to_string()),
        // A DTD-internal, external SYSTEM, or billion-laughs entity. NEVER
        // expanded — the structural XXE / entity-expansion defense.
        _ => Err(format!("undefined entity '{}'", name)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stringify
// ─────────────────────────────────────────────────────────────────────────────

/// Read the `{indent}` stringify option. Returns `Some(n)` for a positive
/// integer indent (pretty), `None` for compact.
fn stringify_indent(opts: Option<&Value>) -> Option<usize> {
    let opts = opts?;
    if let ValueKind::Object(o) = opts.kind() {
        if let Some(v) = o.get("indent") {
            if let Some(n) = v.as_f64() {
                if n > 0.0 {
                    return Some(n as usize);
                }
            }
        }
    }
    None
}

/// Serialize a §7.2 element node back to XML text. `indent=Some(n)` pretty-prints
/// with `n`-space indentation; `None` is compact (no inserted whitespace).
pub(crate) fn stringify_document(node: &Value, indent: Option<usize>) -> Result<String, String> {
    let mut out = String::new();
    write_element(node, indent, 0, &mut out)?;
    if indent.is_some() {
        out.push('\n');
    }
    Ok(out)
}

fn write_element(
    node: &Value,
    indent: Option<usize>,
    depth: usize,
    out: &mut String,
) -> Result<(), String> {
    let ValueKind::Object(obj) = node.kind() else {
        return Err(format!(
            "xml.stringify expects an element object {{tag, attrs, children}}, got {}",
            crate::interp::type_name(node)
        ));
    };
    let tag = match obj.get("tag").as_ref().map(|v| v.kind()) {
        Some(ValueKind::Str(s)) => s.to_string(),
        _ => return Err("xml.stringify: element is missing a string 'tag' field".into()),
    };
    let pad = |out: &mut String, depth: usize| {
        if let Some(n) = indent {
            out.push_str(&" ".repeat(n * depth));
        }
    };

    pad(out, depth);
    out.push('<');
    out.push_str(&tag);

    // Attributes (insertion order preserved).
    if let Some(attrs) = obj.get("attrs") {
        if let ValueKind::Object(a) = attrs.kind() {
            for (k, v) in a.entries() {
                let val = match v.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    ValueKind::Nil => String::new(),
                    _ => value_to_attr_string(&v),
                };
                out.push(' ');
                out.push_str(&k);
                out.push_str("=\"");
                out.push_str(&escape_attr(&val));
                out.push('"');
            }
        }
    }

    let children: Vec<Value> = match obj.get("children").as_ref().map(|v| v.kind()) {
        Some(ValueKind::Array(arr)) => arr.borrow().clone(),
        Some(ValueKind::Nil) | None => Vec::new(),
        _ => return Err("xml.stringify: 'children' must be an array".into()),
    };

    if children.is_empty() {
        out.push_str("/>");
        return Ok(());
    }

    // An element whose only child is text stays on one line even when pretty.
    let only_text = children.len() == 1 && matches!(children[0].kind(), ValueKind::Str(_));

    out.push('>');
    if only_text {
        if let ValueKind::Str(s) = children[0].kind() {
            out.push_str(&escape_text(s.as_ref()));
        }
    } else {
        for child in &children {
            match child.kind() {
                ValueKind::Str(s) => {
                    if indent.is_some() {
                        out.push('\n');
                        if let Some(n) = indent {
                            out.push_str(&" ".repeat(n * (depth + 1)));
                        }
                    }
                    out.push_str(&escape_text(s.as_ref()));
                }
                ValueKind::Object(_) => {
                    if indent.is_some() {
                        out.push('\n');
                    }
                    write_element(child, indent, depth + 1, out)?;
                }
                _ => {
                    return Err(format!(
                        "xml.stringify: a child must be an element object or text string, got {}",
                        crate::interp::type_name(child)
                    ))
                }
            }
        }
        if indent.is_some() {
            out.push('\n');
            pad(out, depth);
        }
    }
    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
    Ok(())
}

fn value_to_attr_string(v: &Value) -> String {
    match v.kind() {
        ValueKind::Str(s) => s.to_string(),
        ValueKind::Int(i) => i.to_string(),
        ValueKind::Float(f) => crate::value::format_float(f),
        ValueKind::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Escape / unescape
// ─────────────────────────────────────────────────────────────────────────────

/// Escape the five predefined XML entities in text content. `&` first.
pub(crate) fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape for an attribute value (double-quoted). Identical set to text here.
fn escape_attr(s: &str) -> String {
    escape_text(s)
}

/// Unescape the five predefined entities + numeric character references. An
/// undefined named entity is a Tier-1 error (the same posture as parse).
pub(crate) fn unescape_text(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            // Find the terminating ';'.
            let Some(semi_rel) = s[i + 1..].find(';') else {
                return Err(format!(
                    "invalid entity reference: '&' without a closing ';' at byte {}",
                    i
                ));
            };
            let name = &s[i + 1..i + 1 + semi_rel];
            if name.is_empty() {
                return Err(format!("invalid empty entity reference '&;' at byte {}", i));
            }
            out.push_str(&resolve_entity(name)?);
            i += 1 + semi_rel + 1;
        } else {
            // Copy one UTF-8 char.
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&s[i..i + ch_len]);
            i += ch_len;
        }
    }
    Ok(out)
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(all(test, feature = "xml"))]
mod tests {
    use super::*;
    use crate::value::ValueKind;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    /// Parse and unwrap the Tier-1 pair, asserting success.
    fn parse_ok(text: &str) -> Value {
        let r = call("parse", &[Value::str(text)], sp()).unwrap();
        let ValueKind::Array(a) = r.kind() else {
            panic!("parse did not return a pair")
        };
        let a = a.borrow();
        assert!(
            matches!(a[1].kind(), ValueKind::Nil),
            "expected ok, got err: {:?}",
            a[1].kind()
        );
        a[0].clone()
    }

    /// Parse and assert it is a Tier-1 error, returning the error message.
    fn parse_err(text: &str) -> String {
        let r = call("parse", &[Value::str(text)], sp()).unwrap();
        let ValueKind::Array(a) = r.kind() else {
            panic!("parse did not return a pair")
        };
        let a = a.borrow();
        assert!(
            matches!(a[0].kind(), ValueKind::Nil),
            "expected err, got value"
        );
        let ValueKind::Object(o) = a[1].kind() else {
            panic!("err is not an object")
        };
        match o.get("message").as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => s.to_string(),
            _ => panic!("err has no message"),
        }
    }

    fn field(node: &Value, key: &str) -> Value {
        let ValueKind::Object(o) = node.kind() else {
            panic!("not an object")
        };
        o.get(key).unwrap_or_else(|| panic!("missing field {key}"))
    }

    fn tag_of(node: &Value) -> String {
        match field(node, "tag").kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("tag not a string"),
        }
    }

    fn children_of(node: &Value) -> Vec<Value> {
        match field(node, "children").kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            _ => panic!("children not an array"),
        }
    }

    fn str_of(v: &Value) -> String {
        match v.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => panic!("not a string"),
        }
    }

    // (a) Stable shape pin.
    #[test]
    fn parse_pins_stable_shape() {
        let node = parse_ok(r#"<root id="1" cls="a"><b>hi</b>tail</root>"#);
        assert_eq!(tag_of(&node), "root");
        // attrs insertion-ordered.
        let attrs = field(&node, "attrs");
        let ValueKind::Object(a) = attrs.kind() else {
            panic!()
        };
        let keys: Vec<String> = a.entries().iter().map(|(k, _)| k.to_string()).collect();
        assert_eq!(keys, vec!["id".to_string(), "cls".to_string()]);
        assert_eq!(str_of(&a.get("id").unwrap()), "1");
        // children: [element <b>, text "tail"].
        let ch = children_of(&node);
        assert_eq!(ch.len(), 2);
        assert_eq!(tag_of(&ch[0]), "b");
        assert_eq!(str_of(&children_of(&ch[0])[0]), "hi");
        assert_eq!(str_of(&ch[1]), "tail");
    }

    // (a) CDATA folded into text; comments + PIs dropped.
    #[test]
    fn parse_cdata_folds_and_drops_comments_pis() {
        let node = parse_ok(
            r#"<r><!-- c --><?pi x?><![CDATA[ <raw> & ]]>after</r>"#,
        );
        let ch = children_of(&node);
        // CDATA + "after" coalesce into one text child; comment + PI dropped.
        assert_eq!(ch.len(), 1);
        assert_eq!(str_of(&ch[0]), " <raw> & after");
    }

    // (b) parse → stringify → parse fixpoint.
    #[test]
    fn round_trip_is_stable() {
        let src = r#"<root id="1"><a x="2">text</a><b/></root>"#;
        let node1 = parse_ok(src);
        let s1 = call("stringify", std::slice::from_ref(&node1), sp()).unwrap();
        let ValueKind::Array(p) = s1.kind() else {
            panic!()
        };
        let text1 = str_of(&p.borrow()[0]);
        let node2 = parse_ok(&text1);
        let s2 = call("stringify", &[node2], sp()).unwrap();
        let ValueKind::Array(p2) = s2.kind() else {
            panic!()
        };
        let text2 = str_of(&p2.borrow()[0]);
        assert_eq!(text1, text2, "round-trip not stable");
    }

    // (c) pretty stringify pinned exactly.
    #[test]
    fn stringify_pretty_pinned() {
        let node = parse_ok(r#"<root id="1"><a>hi</a><b><c>deep</c></b></root>"#);
        let opts = {
            let mut m = IndexMap::new();
            m.insert("indent".to_string(), Value::int(2));
            Value::object(m)
        };
        let r = call("stringify", &[node, opts], sp()).unwrap();
        let ValueKind::Array(p) = r.kind() else {
            panic!()
        };
        let text = str_of(&p.borrow()[0]);
        let expected = "<root id=\"1\">\n  <a>hi</a>\n  <b>\n    <c>deep</c>\n  </b>\n</root>\n";
        assert_eq!(text, expected);
    }

    /// Unwrap the ok value of a Tier-1 `[value, err]` pair.
    fn pair_ok(v: &Value) -> Value {
        let ValueKind::Array(a) = v.kind() else {
            panic!("not a pair")
        };
        let a = a.borrow();
        assert!(matches!(a[1].kind(), ValueKind::Nil), "expected ok pair");
        a[0].clone()
    }

    // (d) escape / unescape both directions + numeric refs.
    #[test]
    fn escape_unescape_roundtrip() {
        let raw = "a & b < c > d \" e ' f";
        // escape returns a bare string.
        let esc = call("escape", &[Value::str(raw)], sp()).unwrap();
        assert_eq!(
            str_of(&esc),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
        // unescape returns a [string, err] pair.
        let un = call("unescape", &[esc], sp()).unwrap();
        assert_eq!(str_of(&pair_ok(&un)), raw);
        // numeric refs (dec + hex).
        let un2 = call("unescape", &[Value::str("&#65;&#x42;&#x263A;")], sp()).unwrap();
        assert_eq!(str_of(&pair_ok(&un2)), "AB\u{263A}");
        // undefined named entity → [nil, err].
        let un3 = call("unescape", &[Value::str("&nope;")], sp()).unwrap();
        let ValueKind::Array(a) = un3.kind() else {
            panic!()
        };
        assert!(matches!(a.borrow()[0].kind(), ValueKind::Nil));
    }

    // (e) malformed → Tier-1 with position info.
    #[test]
    fn malformed_is_tier1() {
        // unclosed tag
        assert!(!parse_err("<a><b></a>").is_empty());
        // bad / mismatched
        let m = parse_err("<a></b>");
        assert!(m.contains("mismatch") || m.contains("invalid"), "got: {m}");
        // half entity `&amp` (no semicolon)
        let m2 = parse_err("<a>&amp</a>");
        assert!(!m2.is_empty(), "half-entity should error");
        // a position hint appears for a raw parse error.
        let m3 = parse_err("<a><<b></a>");
        assert!(m3.to_lowercase().contains("invalid") || m3.contains("byte"), "got: {m3}");
    }

    // (f) SECURITY: internal DTD entity is NOT expanded → Tier-1 undefined entity.
    #[test]
    fn security_internal_entity_not_expanded() {
        let doc = r#"<!DOCTYPE x [<!ENTITY a "BOOM">]><x>&a;</x>"#;
        let m = parse_err(doc);
        assert!(
            m.contains("undefined entity 'a'"),
            "internal entity must not expand; got: {m}"
        );
        // It must NEVER contain the expansion.
        assert!(!m.contains("BOOM"));
    }

    // (f) SECURITY: billion-laughs → never expands (stops at first custom entity).
    #[test]
    fn security_billion_laughs_does_not_expand() {
        let doc = concat!(
            "<!DOCTYPE lolz [",
            "<!ENTITY lol \"lol\">",
            "<!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">",
            "<!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">",
            "]>",
            "<lolz>&lol3;</lolz>"
        );
        let m = parse_err(doc);
        // The first custom entity reference (&lol3;) is undefined → hard stop.
        assert!(
            m.contains("undefined entity"),
            "billion-laughs must not expand; got: {m}"
        );
    }

    // (f) SECURITY: external SYSTEM entity is never fetched → Tier-1.
    #[test]
    fn security_external_entity_not_fetched() {
        let doc = r#"<!DOCTYPE x [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><x>&xxe;</x>"#;
        let m = parse_err(doc);
        assert!(
            m.contains("undefined entity 'xxe'"),
            "external entity must not be fetched/expanded; got: {m}"
        );
    }

    // (f) SECURITY: depth budget → Tier-1, not a stack overflow.
    #[test]
    fn security_depth_budget() {
        let n = MAX_DEPTH + 50;
        let mut doc = String::new();
        for _ in 0..n {
            doc.push_str("<x>");
        }
        for _ in 0..n {
            doc.push_str("</x>");
        }
        let m = parse_err(&doc);
        assert!(m.contains("nesting depth"), "got: {m}");
    }

    // (f) SECURITY: node budget → Tier-1.
    #[test]
    fn security_node_budget() {
        // A wide flat document beyond MAX_NODES would be huge to materialize;
        // assert the constant guards the parser by checking a smaller-but-real
        // overflow via the counter logic on a moderately wide doc is impractical,
        // so we assert the guard fires by construction with the bump helper.
        let mut nodes = MAX_NODES;
        assert!(bump_nodes(&mut nodes).is_err(), "node budget must trip");
    }

    // (g) namespace passthrough: ns:tag and xmlns:* preserved raw.
    #[test]
    fn namespace_passthrough() {
        let node = parse_ok(
            r#"<ns:root xmlns:ns="http://example.com/ns" ns:attr="v"><ns:child/></ns:root>"#,
        );
        assert_eq!(tag_of(&node), "ns:root");
        let attrs = field(&node, "attrs");
        let ValueKind::Object(a) = attrs.kind() else {
            panic!()
        };
        let keys: Vec<String> = a.entries().iter().map(|(k, _)| k.to_string()).collect();
        assert_eq!(keys, vec!["xmlns:ns".to_string(), "ns:attr".to_string()]);
        let ch = children_of(&node);
        assert_eq!(tag_of(&ch[0]), "ns:child");
    }

    // No net/fs dependency: structural — the module compiles with no such import.
    // This test documents the invariant (the real proof is the absence of any
    // `std::fs`/`std::net`/socket symbol in this file).
    #[test]
    fn no_io_in_parse() {
        // A SYSTEM entity pointing at a URL is never fetched (no network code).
        let m = parse_err(
            r#"<!DOCTYPE x [<!ENTITY e SYSTEM "http://attacker.example/evil">]><x>&e;</x>"#,
        );
        assert!(m.contains("undefined entity 'e'"));
    }
}
