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
