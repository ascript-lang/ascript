//! `textDocument/documentColor` + `textDocument/colorPresentation`.
//!
//! An EXTENSIBLE recognizer subsystem (spec §4): an internal `Rgba`, a registry of
//! recognizers each yielding `(ByteSpan, Rgba)`, and a color-sink context registry
//! that gates string-based recognizers to argument positions of color-aware APIs
//! (`color.*` / tui style) so a plain label like `"#100"` never becomes a swatch.

/// 8-bit-per-channel RGBA. The LSP wire `Color` is f32 0..1, so alpha round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba { r, g, b, a: 255 }
    }

    /// The LSP wire color (each channel 0.0..=1.0).
    pub fn to_lsp(self) -> tower_lsp::lsp_types::Color {
        tower_lsp::lsp_types::Color {
            red: self.r as f32 / 255.0,
            green: self.g as f32 / 255.0,
            blue: self.b as f32 / 255.0,
            alpha: self.a as f32 / 255.0,
        }
    }

    /// From an LSP wire color (rounded to nearest 0..=255).
    pub fn from_lsp(c: tower_lsp::lsp_types::Color) -> Self {
        let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgba {
            r: q(c.red),
            g: q(c.green),
            b: q(c.blue),
            a: q(c.alpha),
        }
    }
}

/// Parse a hex color string body (no leading `#`): `rgb`, `rgba`, `rrggbb`,
/// `rrggbbaa`. Returns `None` for any other shape (so `#abcde` is rejected).
pub fn parse_hex_body(body: &str) -> Option<Rgba> {
    let b = body.as_bytes();
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hx = |s: &str| u8::from_str_radix(s, 16).ok();
    let dup = |c: char| {
        let s: String = std::iter::repeat(c).take(2).collect();
        u8::from_str_radix(&s, 16).ok()
    };
    match body.len() {
        3 => {
            let mut it = body.chars();
            Some(Rgba {
                r: dup(it.next()?)?,
                g: dup(it.next()?)?,
                b: dup(it.next()?)?,
                a: 255,
            })
        }
        4 => {
            let mut it = body.chars();
            Some(Rgba {
                r: dup(it.next()?)?,
                g: dup(it.next()?)?,
                b: dup(it.next()?)?,
                a: dup(it.next()?)?,
            })
        }
        6 => Some(Rgba {
            r: hx(&body[0..2])?,
            g: hx(&body[2..4])?,
            b: hx(&body[4..6])?,
            a: 255,
        }),
        8 => Some(Rgba {
            r: hx(&body[0..2])?,
            g: hx(&body[2..4])?,
            b: hx(&body[4..6])?,
            a: hx(&body[6..8])?,
        }),
        _ => None,
    }
}

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;

/// One detected color: the source span of the editable color token + its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorHit {
    pub span: ByteSpan,
    pub color: Rgba,
    /// How the source spells the color (drives format-preserving presentation).
    pub form: ColorForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorForm {
    /// `color.rgb(r,g,b,…)` / `color.bgRgb(…)` — edit the numeric args.
    RgbCall { bg: bool, args: ByteSpan },
    /// A `[r,g,b]` tui style array — edit the array literal.
    ArrayLiteral,
    /// A hex string literal (`"#rrggbb"`), span = the string token incl. quotes.
    HexString { quote: char },
    /// A functional string (`"rgb(...)"`/`"rgba(...)"`/`"hsl(...)"`).
    FunctionalString { quote: char },
}

/// The integer value of a `Number` literal node, if it is a whole 0..=255.
fn u8_literal(node: &ResolvedNode) -> Option<u8> {
    if node.kind() != SyntaxKind::Literal {
        return None;
    }
    let tok = node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Number)?;
    let n: f64 = tok.text().parse().ok()?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return None;
    }
    Some(n as u8)
}

/// Recognizer 1: `color.rgb(r,g,b,text)` / `color.bgRgb(...)` truecolor calls.
fn recognize_rgb_calls(model: &SemanticModel, out: &mut Vec<ColorHit>) {
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        // Callee must be a `MemberExpr` `color.rgb` / `color.bgRgb`.
        let Some(member) = call.children().find(|c| c.kind() == SyntaxKind::MemberExpr) else {
            continue;
        };
        let Some((recv, method)) = member_recv_method(&member) else {
            continue;
        };
        if recv != "color" {
            continue;
        }
        let bg = match method.as_str() {
            "rgb" => false,
            "bgRgb" => true,
            _ => continue,
        };
        let Some(args) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let nums: Vec<ResolvedNode> = args
            .children()
            .filter(|c| c.kind() == SyntaxKind::Literal)
            .cloned()
            .collect();
        if nums.len() < 3 {
            continue;
        }
        let (Some(r), Some(g), Some(b)) =
            (u8_literal(&nums[0]), u8_literal(&nums[1]), u8_literal(&nums[2]))
        else {
            continue;
        };
        // Editable span = from the first numeric arg start to the third arg end.
        let args_span = ByteSpan {
            start: ByteSpan::from(nums[0].text_range()).start,
            end: ByteSpan::from(nums[2].text_range()).end,
        };
        out.push(ColorHit {
            span: args_span,
            color: Rgba::rgb(r, g, b),
            form: ColorForm::RgbCall { bg, args: args_span },
        });
    }
}

/// `recv.method` of a `MemberExpr`: the receiver `NameRef` text + the member ident.
fn member_recv_method(member: &ResolvedNode) -> Option<(String, String)> {
    let recv = member
        .children()
        .find(|c| c.kind() == SyntaxKind::NameRef)
        .and_then(|n| crate::syntax::resolve::ident_text(&n))?;
    // The member name is the trailing `Ident` token of the MemberExpr.
    let method = member
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident)
        .last()?;
    Some((recv, method.text().to_string()))
}

/// Recognizer 2: a `[r,g,b]` integer-triple array literal.
fn recognize_rgb_arrays(model: &SemanticModel, out: &mut Vec<ColorHit>) {
    for arr in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ArrayExpr)
    {
        let elems: Vec<ResolvedNode> = arr
            .children()
            .filter(|c| c.kind() == SyntaxKind::Literal)
            .cloned()
            .collect();
        if elems.len() != 3 {
            continue;
        }
        let (Some(r), Some(g), Some(b)) =
            (u8_literal(&elems[0]), u8_literal(&elems[1]), u8_literal(&elems[2]))
        else {
            continue;
        };
        // Only when EVERY child of the array is one of the three numeric literals
        // (no extra elements / spreads) — keeps it an unambiguous color triple.
        if arr
            .children()
            .any(|c| c.kind() != SyntaxKind::Literal)
        {
            continue;
        }
        out.push(ColorHit {
            span: ByteSpan::from(arr.text_range()),
            color: Rgba::rgb(r, g, b),
            form: ColorForm::ArrayLiteral,
        });
    }
}

/// A color-SINK descriptor: a `recv.method` whose given argument INDICES accept a
/// color string. Today: `color.rgb`/`color.bgRgb` (none — those take numbers, not
/// color strings) and the tui style fields are object-valued, so string color
/// SINKS are reserved for the coming CSS/HTML modules. The registry is the single
/// extension point: add a row, no recognizer change.
struct ColorSink {
    recv: &'static str,
    method: &'static str,
    /// Argument indices (0-based) that accept a color string.
    color_args: &'static [usize],
}

/// The color-sink registry. EMPTY of string-arg sinks today (the truecolor APIs
/// take numbers); it exists so a future `css.color("#fff")` adds one row here and
/// the hex-string recognizer lights up with no other change. Object-FIELD sinks
/// (tui `fg`/`bg`) are handled by the value-position array recognizer, not here.
const COLOR_SINKS: &[ColorSink] = &[];

/// Whether the string token at `span` sits in a registered color-sink argument
/// position. Walks up to the enclosing `CallExpr` and checks the callee + arg
/// index against `COLOR_SINKS`.
fn string_in_color_sink(_model: &SemanticModel, str_node: &ResolvedNode) -> bool {
    // The string's enclosing `ArgList` and `CallExpr`.
    let Some(arglist) = ancestor(str_node, SyntaxKind::ArgList) else {
        return false;
    };
    let Some(call) = ancestor(&arglist, SyntaxKind::CallExpr) else {
        return false;
    };
    let Some(member) = call.children().find(|c| c.kind() == SyntaxKind::MemberExpr) else {
        return false;
    };
    let Some((recv, method)) = member_recv_method(&member) else {
        return false;
    };
    // The string's arg index = its position among the arglist's expression children.
    let target = ByteSpan::from(str_node.text_range());
    let mut idx = 0usize;
    for c in arglist.children().filter(|c| is_expr_node(c.kind())) {
        let cs = ByteSpan::from(c.text_range());
        if cs == target || (target.start >= cs.start && target.end <= cs.end) {
            break;
        }
        idx += 1;
    }
    COLOR_SINKS
        .iter()
        .any(|s| s.recv == recv && s.method == method && s.color_args.contains(&idx))
}

/// The nearest ancestor of `node` with `kind`.
fn ancestor(node: &ResolvedNode, kind: SyntaxKind) -> Option<ResolvedNode> {
    let mut cur = node.parent().cloned();
    while let Some(n) = cur {
        if n.kind() == kind {
            return Some(n);
        }
        cur = n.parent().cloned();
    }
    None
}

/// Whether `kind` is an expression node (an arg slot). Mirror the set used by
/// `crate::check::rules::is_expr_kind` (re-use it if visible).
fn is_expr_node(kind: SyntaxKind) -> bool {
    crate::check::rules::is_expr_kind(kind)
}

/// The string body (unquoted) + quote char of a `Str` token node.
fn string_body(str_node: &ResolvedNode) -> Option<(String, char)> {
    let tok = str_node
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Str)?;
    let raw = tok.text();
    let q = raw.chars().next()?;
    if (q != '"' && q != '\'') || !raw.ends_with(q) || raw.len() < 2 {
        return None;
    }
    Some((raw[1..raw.len() - 1].to_string(), q))
}

/// Recognizer 3 + 4: hex (`#rgb`/`#rrggbb`/…) and functional (`rgb()`/`hsl()`)
/// strings — gated by the color-sink context UNLESS `detect_everywhere`.
fn recognize_color_strings(
    model: &SemanticModel,
    detect_everywhere: bool,
    out: &mut Vec<ColorHit>,
) {
    for s in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Literal)
    {
        let Some((body, quote)) = string_body(&s) else {
            continue;
        };
        if !detect_everywhere && !string_in_color_sink(model, &s) {
            continue; // THE #100 GUARD: an ungated label string is never a swatch.
        }
        let span = ByteSpan::from(s.text_range());
        if let Some(hex) = body.strip_prefix('#').and_then(parse_hex_body) {
            out.push(ColorHit { span, color: hex, form: ColorForm::HexString { quote } });
        } else if let Some(c) = parse_functional(&body) {
            out.push(ColorHit {
                span,
                color: c,
                form: ColorForm::FunctionalString { quote },
            });
        }
    }
}

/// Parse `rgb(r,g,b)` / `rgba(r,g,b,a)` / `hsl(h,s%,l%)` / `hsla(...)`. Returns
/// `None` for any other shape.
fn parse_functional(body: &str) -> Option<Rgba> {
    let body = body.trim();
    let (name, rest) = body.split_once('(')?;
    let inner = rest.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    match name.trim() {
        "rgb" if parts.len() == 3 => Some(Rgba::rgb(u8c(parts[0])?, u8c(parts[1])?, u8c(parts[2])?)),
        "rgba" if parts.len() == 4 => Some(Rgba {
            r: u8c(parts[0])?,
            g: u8c(parts[1])?,
            b: u8c(parts[2])?,
            a: alpha_u8(parts[3])?,
        }),
        "hsl" if parts.len() == 3 => {
            Some(hsl_to_rgba(deg(parts[0])?, pct(parts[1])?, pct(parts[2])?, 255))
        }
        "hsla" if parts.len() == 4 => Some(hsl_to_rgba(
            deg(parts[0])?,
            pct(parts[1])?,
            pct(parts[2])?,
            alpha_u8(parts[3])?,
        )),
        _ => None,
    }
}

fn u8c(s: &str) -> Option<u8> {
    let n: f64 = s.parse().ok()?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return None;
    }
    Some(n as u8)
}
fn alpha_u8(s: &str) -> Option<u8> {
    let n: f64 = s.parse().ok()?;
    if !(0.0..=1.0).contains(&n) {
        return None;
    }
    Some((n * 255.0).round() as u8)
}
fn deg(s: &str) -> Option<f64> {
    s.trim_end_matches("deg").trim().parse().ok()
}
fn pct(s: &str) -> Option<f64> {
    let s = s.strip_suffix('%')?;
    let n: f64 = s.trim().parse().ok()?;
    Some(n / 100.0)
}

/// HSL (h in degrees, s/l in 0..1) → RGBA with the given alpha.
fn hsl_to_rgba(h: f64, s: f64, l: f64, a: u8) -> Rgba {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba { r: q(r1), g: q(g1), b: q(b1), a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_bodies_parse_all_shapes() {
        assert_eq!(parse_hex_body("f00"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_hex_body("ff0000"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_hex_body("00ff0080").unwrap().a, 0x80);
        assert_eq!(parse_hex_body("100"), Some(Rgba::rgb(0x11, 0x00, 0x00)));
        // Malformed shapes are rejected.
        assert_eq!(parse_hex_body("xyz"), None);
        assert_eq!(parse_hex_body("abcde"), None);
    }

    #[test]
    fn rgba_round_trips_through_lsp() {
        let c = Rgba { r: 10, g: 20, b: 30, a: 128 };
        assert_eq!(Rgba::from_lsp(c.to_lsp()), c);
    }
}

#[cfg(test)]
mod recognizer_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn detects_color_rgb_call() {
        let m = model("let s = color.rgb(255, 0, 0, \"x\")\n");
        let mut out = Vec::new();
        recognize_rgb_calls(&m, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].color, Rgba::rgb(255, 0, 0));
        assert!(matches!(out[0].form, ColorForm::RgbCall { bg: false, .. }));
        // The swatch span covers the three numeric channels.
        assert_eq!(&m.text[out[0].span.start..out[0].span.end], "255, 0, 0");
    }

    #[test]
    fn detects_bg_rgb_call() {
        let m = model("let s = color.bgRgb(0, 128, 255, \"x\")\n");
        let mut out = Vec::new();
        recognize_rgb_calls(&m, &mut out);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].form, ColorForm::RgbCall { bg: true, .. }));
    }

    #[test]
    fn detects_rgb_triple_array() {
        let m = model("let style = { fg: [10, 20, 30] }\n");
        let mut out = Vec::new();
        recognize_rgb_arrays(&m, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].color, Rgba::rgb(10, 20, 30));
    }

    #[test]
    fn ignores_non_color_array() {
        // A 4-element array is not an rgb triple.
        let m = model("let xs = [1, 2, 3, 4]\n");
        let mut out = Vec::new();
        recognize_rgb_arrays(&m, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }
}

#[cfg(test)]
mod string_context_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn label_hash_100_is_not_a_color() {
        // The `#100` false-positive case from the spec (a 3-digit hex SHAPE that is
        // really a label). Ungated, it must produce NO swatch.
        let m = model("let p = { label: \"#100\" }\n");
        let mut out = Vec::new();
        recognize_color_strings(&m, /*detect_everywhere=*/ false, &mut out);
        assert!(out.is_empty(), "a label string must not be a color: {out:?}");
    }

    #[test]
    fn corpus_typed_fields_hash_100_not_flagged() {
        // The real `examples/typed_fields.as` has `p.label == "#100"` — guard against
        // a regression that would surface a swatch on that comparison string.
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/typed_fields.as"
        ))
        .expect("read examples/typed_fields.as");
        assert!(src.contains("\"#100\""), "fixture drifted: no #100 literal");
        let m = model(&src);
        let mut out = Vec::new();
        recognize_color_strings(&m, /*detect_everywhere=*/ false, &mut out);
        assert!(
            out.iter().all(|h| &m.text[h.span.start..h.span.end] != "\"#100\""),
            "#100 must not be a color: {out:?}"
        );
    }

    #[test]
    fn detect_everywhere_opt_in_finds_hash_100() {
        let m = model("let p = { label: \"#100\" }\n");
        let mut out = Vec::new();
        recognize_color_strings(&m, /*detect_everywhere=*/ true, &mut out);
        assert_eq!(out.len(), 1, "opt-in detects the hex shape: {out:?}");
        assert_eq!(out[0].color, Rgba::rgb(0x11, 0x00, 0x00));
    }

    #[test]
    fn functional_rgb_string_parses() {
        assert_eq!(parse_functional("rgb(255, 0, 0)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_functional("rgba(0, 0, 0, 0.5)").unwrap().a, 128);
        assert_eq!(parse_functional("hsl(0, 100%, 50%)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_functional("nope(1,2)"), None);
    }
}
