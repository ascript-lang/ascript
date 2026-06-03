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

/// Parse a numeric literal's *text* into the exact `f64` value, covering hex
/// (`0x..`/`0X..`), binary (`0b..`/`0B..`), and decimal/float/scientific forms,
/// with underscore digit separators stripped first in all forms. Hex/binary
/// digits parse via `u64::from_str_radix` then cast to `f64`; everything else
/// goes through `f64::parse`.
///
/// Returns `None` only on text the lexer would not have accepted (an empty
/// radix body, or an unparseable decimal) — for a lexer-validated token this
/// never fires; the compiler surfaces a `None` as an internal `CompileError`.
pub(crate) fn parse_number_text(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X' | b'b' | b'B') {
        let radix = if matches!(bytes[1], b'x' | b'X') { 16 } else { 2 };
        let digits: String = text[2..].chars().filter(|&c| c != '_').collect();
        if digits.is_empty() {
            return None;
        }
        return u64::from_str_radix(&digits, radix).ok().map(|n| n as f64);
    }
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    cleaned.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_all_forms() {
        assert_eq!(parse_number_text("0xff"), Some(255.0));
        assert_eq!(parse_number_text("0XFF"), Some(255.0));
        assert_eq!(parse_number_text("0X1F"), Some(31.0));
        assert_eq!(parse_number_text("0b1010"), Some(10.0));
        assert_eq!(parse_number_text("0B11"), Some(3.0));
        assert_eq!(parse_number_text("1e3"), Some(1000.0));
        assert_eq!(parse_number_text("1.5e-3"), Some(0.0015));
        assert_eq!(parse_number_text("2.5e-2"), Some(0.025));
        assert_eq!(parse_number_text("1_000_000"), Some(1_000_000.0));
        assert_eq!(parse_number_text("0xFF_FF"), Some(65535.0));
        assert_eq!(parse_number_text("12.5"), Some(12.5));
        assert_eq!(parse_number_text("0"), Some(0.0));
        assert_eq!(parse_number_text("0x"), None);
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
