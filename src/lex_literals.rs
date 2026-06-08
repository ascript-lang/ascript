//! Shared text→value helpers for numeric and string/template literals.
//!
//! This is the SINGLE source of truth for turning a literal's *text* into its
//! runtime value, used by BOTH the legacy lexer (`src/lexer.rs`, which scans raw
//! source and fuses scan+unescape) and the bytecode compiler
//! (`src/compile/mod.rs`, which receives CST token text). Number forms
//! (hex/binary/scientific/underscore) and escape handling are exactly where two
//! independent copies drift silently, so they live here once.
//!
//! The legacy lexer's *scanning* (finding token boundaries) stays in the lexer;
//! only the pure text→value translation is shared. The escape translation
//! (`escape_char`) is called by the lexer's scan loop directly, so the unescape
//! body loops (`unescape_str_body` / `unescape_template_body`) and the lexer
//! produce byte-identical results.

/// Translate the character following a `\` into its escaped value. Shared by
/// all three string forms (`"..."`, `'...'`, and `` `...` ``). Unknown escapes
/// pass through leniently (`\<other>` -> `<other>`). AScript has NO
/// `\u`/`\x`/numeric escapes, so they fall through to the passthrough.
/// Template-specific escapes (`` \` `` and `\$`) are handled by the template
/// caller before reaching here.
pub(crate) fn escape_char(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        '\\' => '\\',
        '"' => '"',
        '\'' => '\'',
        other => other,
    }
}

/// Unescape a quoted-string *body* (the text BETWEEN the quotes, quotes already
/// stripped) via [`escape_char`]. A trailing lone `\` (which cannot occur in a
/// lexer-accepted, terminated token) is kept verbatim, matching the legacy
/// scan's behavior.
pub(crate) fn unescape_str_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(e) => out.push(escape_char(e)),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Unescape a template-chunk *body* (delimiters already stripped) into its
/// literal contents. `` \` `` → `` ` `` and `\$` → `$` are template-specific;
/// everything else shares [`escape_char`]. A trailing lone `\` is kept verbatim.
pub(crate) fn unescape_template_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('`') => out.push('`'),
                Some('$') => out.push('$'),
                Some(other) => out.push(escape_char(other)),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The runtime subtype a numeric literal denotes (NUM §3.1): an integer literal
/// (no `.`, no exponent) is an `int`; a literal with a `.` or an exponent is a
/// `float`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NumLit {
    Int(i64),
    Float(f64),
}

/// Why a numeric literal's text could not be turned into a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum NumLitError {
    /// Text the lexer would not have accepted (empty radix body, unparseable
    /// decimal). For a lexer-validated token this never fires.
    Invalid,
    /// A syntactically-valid integer literal whose value does not fit in `i64`
    /// (NUM §3.1 — a clean lex/parse error, never a silent wrap or float
    /// fallback).
    OutOfRange,
}

impl NumLitError {
    /// The user-facing diagnostic message for this error, given the radix label
    /// the caller would otherwise use for an [`NumLitError::Invalid`].
    pub(crate) fn message(self, invalid_label: &str) -> &str {
        match self {
            NumLitError::Invalid => invalid_label,
            NumLitError::OutOfRange => "integer literal out of range for int (i64)",
        }
    }
}

/// Parse a numeric literal's *text* into its subtype value (NUM §3.1), covering
/// hex (`0x..`/`0X..`), binary (`0b..`/`0B..`), octal (`0o..`/`0O..`), and
/// decimal/float/scientific forms, with underscore digit separators stripped
/// first in all forms.
///
/// Subtype rule: a literal with NO `.` and NO exponent is an `int`; one with a
/// `.` or an exponent is a `float`. Hex/binary/octal are bit patterns and so are
/// always `int`. Integer literals parse via `i64::from_str_radix` (radix 16/2/8)
/// or `i64::parse` (decimal); a value that overflows `i64` is
/// [`NumLitError::OutOfRange`] — never a silent wrap or float fallback. Floats go
/// through `f64::parse`.
pub(crate) fn parse_number_text(text: &str) -> Result<NumLit, NumLitError> {
    let bytes = text.as_bytes();
    // Radix-prefixed integer literals: hex / binary / octal. Always `int`.
    if bytes.len() >= 2 && bytes[0] == b'0' {
        let radix = match bytes[1] {
            b'x' | b'X' => Some(16u32),
            b'b' | b'B' => Some(2u32),
            b'o' | b'O' => Some(8u32),
            _ => None,
        };
        if let Some(radix) = radix {
            let digits: String = text[2..].chars().filter(|&c| c != '_').collect();
            if digits.is_empty() {
                return Err(NumLitError::Invalid);
            }
            return match i64::from_str_radix(&digits, radix) {
                Ok(n) => Ok(NumLit::Int(n)),
                // `from_str_radix` fails on either bad digits (impossible — the
                // lexer validated them) or overflow. Treat as out-of-range.
                Err(_) => Err(NumLitError::OutOfRange),
            };
        }
    }
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    // A `.` or an exponent makes it a float; otherwise it is an integer literal.
    let is_float = cleaned.contains('.') || cleaned.contains('e') || cleaned.contains('E');
    if is_float {
        return cleaned.parse::<f64>().map(NumLit::Float).map_err(|_| NumLitError::Invalid);
    }
    match cleaned.parse::<i64>() {
        Ok(n) => Ok(NumLit::Int(n)),
        // Distinguish overflow (a valid all-digit literal too big for i64) from
        // genuinely malformed text.
        Err(e) => {
            if *e.kind() == std::num::IntErrorKind::PosOverflow
                || *e.kind() == std::num::IntErrorKind::NegOverflow
            {
                Err(NumLitError::OutOfRange)
            } else {
                Err(NumLitError::Invalid)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_all_forms() {
        // Radix-prefixed and bare-digit forms are ints.
        assert_eq!(parse_number_text("0xff"), Ok(NumLit::Int(255)));
        assert_eq!(parse_number_text("0XFF"), Ok(NumLit::Int(255)));
        assert_eq!(parse_number_text("0X1F"), Ok(NumLit::Int(31)));
        assert_eq!(parse_number_text("0b1010"), Ok(NumLit::Int(10)));
        assert_eq!(parse_number_text("0B11"), Ok(NumLit::Int(3)));
        // Octal (NUM §3.1, new).
        assert_eq!(parse_number_text("0o17"), Ok(NumLit::Int(15)));
        assert_eq!(parse_number_text("0O17"), Ok(NumLit::Int(15)));
        // `.` or exponent → float.
        assert_eq!(parse_number_text("1e3"), Ok(NumLit::Float(1000.0)));
        assert_eq!(parse_number_text("1.5e-3"), Ok(NumLit::Float(0.0015)));
        assert_eq!(parse_number_text("2.5e-2"), Ok(NumLit::Float(0.025)));
        assert_eq!(parse_number_text("12.5"), Ok(NumLit::Float(12.5)));
        // Underscores stripped; bare digit run → int.
        assert_eq!(parse_number_text("1_000_000"), Ok(NumLit::Int(1_000_000)));
        assert_eq!(parse_number_text("0xFF_FF"), Ok(NumLit::Int(65535)));
        assert_eq!(parse_number_text("255"), Ok(NumLit::Int(255)));
        assert_eq!(parse_number_text("0"), Ok(NumLit::Int(0)));
        // Empty radix body → invalid.
        assert_eq!(parse_number_text("0x"), Err(NumLitError::Invalid));
    }

    #[test]
    fn parse_number_int_vs_float_discrimination() {
        assert_eq!(parse_number_text("5"), Ok(NumLit::Int(5)));
        assert_eq!(parse_number_text("5.0"), Ok(NumLit::Float(5.0)));
        // An integral exponent is still a float (NUM §3.1).
        assert_eq!(parse_number_text("1e9"), Ok(NumLit::Float(1e9)));
    }

    #[test]
    fn parse_number_out_of_range_is_error() {
        // i64::MAX is 9223372036854775807; this is one past it.
        assert_eq!(
            parse_number_text("9223372036854775808"),
            Err(NumLitError::OutOfRange)
        );
        // i64::MAX itself parses fine.
        assert_eq!(
            parse_number_text("9223372036854775807"),
            Ok(NumLit::Int(i64::MAX))
        );
        // Hex overflow.
        assert_eq!(
            parse_number_text("0xFFFFFFFFFFFFFFFF"),
            Err(NumLitError::OutOfRange)
        );
    }

    #[test]
    fn unescape_str_body_full_escape_set() {
        assert_eq!(unescape_str_body(r"a\nb"), "a\nb");
        assert_eq!(unescape_str_body(r"t\ta"), "t\ta");
        assert_eq!(unescape_str_body(r"r\ra"), "r\ra");
        assert_eq!(unescape_str_body(r#"q\"x"#), "q\"x");
        assert_eq!(unescape_str_body(r"b\\s"), "b\\s");
        assert_eq!(unescape_str_body(r"n\0e"), "n\0e");
        assert_eq!(unescape_str_body(r"\t\r\0"), "\t\r\0");
        assert_eq!(unescape_str_body(r"\'q\'"), "'q'");
        // Lenient passthrough of an unknown escape: `\q` -> `q`.
        assert_eq!(unescape_str_body(r"x\qy"), "xqy");
    }

    #[test]
    fn unescape_template_body_escapes() {
        assert_eq!(unescape_template_body("plain"), "plain");
        assert_eq!(unescape_template_body(r"a\`b"), "a`b");
        assert_eq!(unescape_template_body(r"a\$b"), "a$b");
        assert_eq!(unescape_template_body(r"a\nb"), "a\nb");
    }
}
