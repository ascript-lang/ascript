//! Hand-written lexer. Produces tokens with char-offset spans.

use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

enum TemplateChunk {
    Full,  // `...`           (no interpolation)
    Start, // `...${          (more follows)
}

/// Translate the character following a `\` into its escaped value. Shared by
/// all three string forms (`"..."`, `'...'`, and `` `...` ``). Unknown escapes
/// pass through leniently (`\<other>` -> `<other>`), matching the original
/// template behavior. Template-specific escapes (`` \` `` and `\$`) are
/// handled by the caller before reaching here (they fall through to the
/// lenient passthrough anyway, but listing them keeps intent explicit).
fn escape_char(c: char) -> char {
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

/// Read a quoted string body starting just after the opening `quote`, scanning
/// until the matching unescaped `quote`. Advances `i` past the closing quote.
/// Backslash escapes are translated via `escape_char`.
fn lex_quoted(
    chars: &[char],
    i: &mut usize,
    start: usize,
    quote: char,
) -> Result<String, AsError> {
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == quote {
            *i += 1; // consume closing quote
            return Ok(s);
        }
        if c == '\\' && *i + 1 < chars.len() {
            *i += 1;
            s.push(escape_char(chars[*i]));
            *i += 1;
            continue;
        }
        s.push(c);
        *i += 1;
    }
    Err(AsError::at("unterminated string", Span::new(start, *i)))
}

/// Read template text starting just after a backtick (or after `}` that closes
/// an interpolation). Advances `i` past the terminating `` ` `` or `${`.
fn lex_template_chunk(
    chars: &[char],
    i: &mut usize,
    start: usize,
) -> Result<(String, TemplateChunk), AsError> {
    let mut text = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == '`' {
            *i += 1;
            return Ok((text, TemplateChunk::Full));
        }
        if c == '$' && *i + 1 < chars.len() && chars[*i + 1] == '{' {
            *i += 2;
            return Ok((text, TemplateChunk::Start));
        }
        if c == '\\' && *i + 1 < chars.len() {
            // Escapes inside templates. `` \` `` and `\$` are template-specific
            // (they escape the interpolation/terminator syntax); everything
            // else shares the common escape set via `escape_char`.
            *i += 1;
            let e = chars[*i];
            text.push(match e {
                '`' => '`',
                '$' => '$',
                other => escape_char(other),
            });
            *i += 1;
            continue;
        }
        text.push(c);
        *i += 1;
    }
    Err(AsError::at("unterminated template string", Span::new(start, *i)))
}

pub fn lex(src: &str) -> Result<Vec<Token>, AsError> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut brace_depth = 0usize;
    let mut template_stack: Vec<usize> = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        let start = i;

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        match c {
            '+' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::PlusEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Plus, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '-' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::MinusEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Minus, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    tokens.push(Token { tok: Tok::StarStar, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::StarEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Star, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::BangEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Bang, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::EqEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { tok: Tok::FatArrow, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Eq, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::Le, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Lt, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::Ge, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Gt, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token { tok: Tok::AmpAmp, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at("unexpected character '&'", Span::new(start, start + 1)));
                }
            }
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token { tok: Tok::PipePipe, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Pipe, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token { tok: Tok::QuestionQuestion, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::QuestionDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Question, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '/' {
                    // line comment `// ...` to end of line (spec grammar: line_comment)
                    i += 2;
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                } else if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // block comment `/* ... */` (spec grammar: block_comment)
                    i += 2;
                    let mut closed = false;
                    while i + 1 < chars.len() {
                        if chars[i] == '*' && chars[i + 1] == '/' {
                            closed = true;
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    if !closed {
                        return Err(AsError::at(
                            "unterminated block comment",
                            Span::new(start, i),
                        ));
                    }
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::SlashEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Slash, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::DotDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Dot, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '%' => push(&mut tokens, Tok::Percent, start, &mut i),
            '(' => push(&mut tokens, Tok::LParen, start, &mut i),
            ')' => push(&mut tokens, Tok::RParen, start, &mut i),
            ',' => push(&mut tokens, Tok::Comma, start, &mut i),
            ';' => push(&mut tokens, Tok::Semicolon, start, &mut i),
            ':' => push(&mut tokens, Tok::Colon, start, &mut i),
            '{' => {
                brace_depth += 1;
                push(&mut tokens, Tok::LBrace, start, &mut i);
            }
            '}' => {
                if let Some(&open_depth) = template_stack.last() {
                    if brace_depth == open_depth {
                        // This `}` closes a template interpolation.
                        template_stack.pop();
                        i += 1;
                        let (text, kind) = lex_template_chunk(&chars, &mut i, start)?;
                        match kind {
                            TemplateChunk::Full => tokens.push(Token {
                                tok: Tok::TemplateEnd(text),
                                span: Span::new(start, i),
                            }),
                            TemplateChunk::Start => {
                                tokens.push(Token {
                                    tok: Tok::TemplateMiddle(text),
                                    span: Span::new(start, i),
                                });
                                template_stack.push(brace_depth);
                            }
                        }
                        continue;
                    }
                }
                brace_depth = brace_depth.saturating_sub(1);
                push(&mut tokens, Tok::RBrace, start, &mut i);
            }
            '[' => push(&mut tokens, Tok::LBracket, start, &mut i),
            ']' => push(&mut tokens, Tok::RBracket, start, &mut i),
            '"' => {
                i += 1;
                let s = lex_quoted(&chars, &mut i, start, '"')?;
                tokens.push(Token { tok: Tok::Str(s), span: Span::new(start, i) });
            }
            '\'' => {
                i += 1;
                let s = lex_quoted(&chars, &mut i, start, '\'')?;
                tokens.push(Token { tok: Tok::Str(s), span: Span::new(start, i) });
            }
            '`' => {
                i += 1;
                let (text, kind) = lex_template_chunk(&chars, &mut i, start)?;
                match kind {
                    TemplateChunk::Full => tokens.push(Token {
                        tok: Tok::TemplateStr(text),
                        span: Span::new(start, i),
                    }),
                    TemplateChunk::Start => {
                        tokens.push(Token {
                            tok: Tok::TemplateStart(text),
                            span: Span::new(start, i),
                        });
                        template_stack.push(brace_depth);
                    }
                }
            }
            c if c.is_ascii_digit() => {
                // note: leading-dot floats (e.g. `.5`) are intentionally
                // unsupported — this arm is only entered on a digit, and a
                // leading `.` is reserved for member access / the `..` range
                // operator. Write `0.5` instead.
                let mut j = i;
                // Hex / binary prefixes must be checked FIRST: `0x..` / `0b..`.
                // (A bare `0`, `0.5`, `0e1` fall through to the decimal scan.)
                if chars[i] == '0'
                    && i + 1 < chars.len()
                    && matches!(chars[i + 1], 'x' | 'X' | 'b' | 'B')
                {
                    let radix_char = chars[i + 1];
                    let (radix, is_digit): (u32, fn(char) -> bool) = match radix_char {
                        'x' | 'X' => (16, |d| d.is_ascii_hexdigit()),
                        _ => (2, |d| d == '0' || d == '1'),
                    };
                    j = i + 2;
                    while j < chars.len() && (is_digit(chars[j]) || chars[j] == '_') {
                        j += 1;
                    }
                    let span = Span::new(i, j);
                    let digits: String =
                        chars[i + 2..j].iter().filter(|&&ch| ch != '_').collect();
                    let label = if radix == 16 {
                        "invalid hex number literal"
                    } else {
                        "invalid binary number literal"
                    };
                    if digits.is_empty() {
                        return Err(AsError::at(label, span));
                    }
                    let n = u64::from_str_radix(&digits, radix)
                        .map_err(|_| AsError::at(label, span))?
                        as f64;
                    tokens.push(Token { tok: Tok::Number(n), span });
                    i = j;
                } else {
                    // Decimal / float / scientific: \d[\d_]* (.\d[\d_]*)? ([eE][+-]?\d+)?
                    let is_dec = |d: char| d.is_ascii_digit() || d == '_';
                    while j < chars.len() && is_dec(chars[j]) {
                        j += 1;
                    }
                    // Optional fraction — only when `.` is followed by a digit,
                    // so `0..5` (range) and `a.0` (member) are preserved.
                    if j + 1 < chars.len() && chars[j] == '.' && chars[j + 1].is_ascii_digit() {
                        j += 1;
                        while j < chars.len() && is_dec(chars[j]) {
                            j += 1;
                        }
                    }
                    // Optional exponent: e/E, optional sign, then a digit run.
                    if j < chars.len() && matches!(chars[j], 'e' | 'E') {
                        let after = j + 1;
                        let exp_ok = if after < chars.len() && chars[after].is_ascii_digit() {
                            true
                        } else {
                            after + 1 < chars.len()
                                && matches!(chars[after], '+' | '-')
                                && chars[after + 1].is_ascii_digit()
                        };
                        if exp_ok {
                            j += 1; // consume e/E
                            if matches!(chars[j], '+' | '-') {
                                j += 1;
                            }
                            while j < chars.len() && chars[j].is_ascii_digit() {
                                j += 1;
                            }
                        }
                    }
                    let span = Span::new(i, j);
                    let text: String = chars[i..j].iter().filter(|&&ch| ch != '_').collect();
                    let n: f64 = text
                        .parse()
                        .map_err(|_| AsError::at("invalid number", span))?;
                    tokens.push(Token { tok: Tok::Number(n), span });
                    i = j;
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut j = i;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let text: String = chars[i..j].iter().collect();
                let tok = match text.as_str() {
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "nil" => Tok::Nil,
                    "let" => Tok::Let,
                    "const" => Tok::Const,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "for" => Tok::For,
                    "in" => Tok::In,
                    "of" => Tok::Of,
                    "return" => Tok::Return,
                    "break" => Tok::Break,
                    "continue" => Tok::Continue,
                    "fn" => Tok::Fn,
                    "enum" => Tok::Enum,
                    "match" => Tok::Match,
                    "class" => Tok::Class,
                    "import" => Tok::Import,
                    "export" => Tok::Export,
                    "async" => Tok::Async,
                    "await" => Tok::Await,
                    _ => Tok::Ident(text),
                };
                tokens.push(Token { tok, span: Span::new(i, j) });
                i = j;
            }
            other => {
                return Err(AsError::at(
                    format!("unexpected character '{}'", other),
                    Span::new(start, start + 1),
                ));
            }
        }
    }

    tokens.push(Token { tok: Tok::Eof, span: Span::new(chars.len(), chars.len()) });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, tok: Tok, start: usize, i: &mut usize) {
    tokens.push(Token { tok, span: Span::new(start, start + 1) });
    *i += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_question_variants() {
        assert_eq!(kinds("a ? b ?? c?.d"),
            vec![
                Tok::Ident("a".into()), Tok::Question,
                Tok::Ident("b".into()), Tok::QuestionQuestion,
                Tok::Ident("c".into()), Tok::QuestionDot, Tok::Ident("d".into()),
                Tok::Eof,
            ]);
    }

    #[test]
    fn stray_close_brace_does_not_panic() {
        // A stray `}` must lex cleanly (no usize underflow panic); the parser
        // rejects it later as an unexpected token.
        assert_eq!(kinds("}"), vec![Tok::RBrace, Tok::Eof]);
    }

    #[test]
    fn lexes_arithmetic() {
        assert_eq!(
            kinds("1 + 2 * 3"),
            vec![
                Tok::Number(1.0),
                Tok::Plus,
                Tok::Number(2.0),
                Tok::Star,
                Tok::Number(3.0),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_strings_and_keywords() {
        assert_eq!(
            kinds("\"hi\" true nil"),
            vec![Tok::Str("hi".into()), Tok::True, Tok::Nil, Tok::Eof]
        );
    }

    #[test]
    fn lexes_multi_char_operators() {
        assert_eq!(
            kinds("a ** b == c != d <= e >= f && g || h ?? i"),
            vec![
                Tok::Ident("a".into()), Tok::StarStar, Tok::Ident("b".into()),
                Tok::EqEq, Tok::Ident("c".into()),
                Tok::BangEq, Tok::Ident("d".into()),
                Tok::Le, Tok::Ident("e".into()),
                Tok::Ge, Tok::Ident("f".into()),
                Tok::AmpAmp, Tok::Ident("g".into()),
                Tok::PipePipe, Tok::Ident("h".into()),
                Tok::QuestionQuestion, Tok::Ident("i".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_single_char_comparisons_and_bang() {
        assert_eq!(
            kinds("!a < b > c"),
            vec![
                Tok::Bang, Tok::Ident("a".into()),
                Tok::Lt, Tok::Ident("b".into()),
                Tok::Gt, Tok::Ident("c".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_compound_assignment_operators() {
        assert_eq!(
            kinds("a += b -= c *= d /= e"),
            vec![
                Tok::Ident("a".into()), Tok::PlusEq, Tok::Ident("b".into()),
                Tok::MinusEq, Tok::Ident("c".into()),
                Tok::StarEq, Tok::Ident("d".into()),
                Tok::SlashEq, Tok::Ident("e".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_fat_arrow() {
        assert_eq!(
            kinds("x => x"),
            vec![Tok::Ident("x".into()), Tok::FatArrow, Tok::Ident("x".into()), Tok::Eof]
        );
    }

    #[test]
    fn lexes_plain_template() {
        assert_eq!(kinds("`hello`"), vec![Tok::TemplateStr("hello".into()), Tok::Eof]);
    }

    #[test]
    fn lexes_interpolated_template() {
        // `a${x}b`  ->  Start("a") Ident(x) End("b")
        assert_eq!(
            kinds("`a${x}b`"),
            vec![
                Tok::TemplateStart("a".into()),
                Tok::Ident("x".into()),
                Tok::TemplateEnd("b".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn rejects_unterminated_string() {
        let err = lex("\"oops").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn lexes_single_quoted_string() {
        assert_eq!(kinds("'single'"), vec![Tok::Str("single".into()), Tok::Eof]);
    }

    #[test]
    fn double_quote_escape_inside_double_string() {
        // source: "a\"b"
        assert_eq!(kinds("\"a\\\"b\""), vec![Tok::Str("a\"b".into()), Tok::Eof]);
    }

    #[test]
    fn newline_escape_in_double_string() {
        // source: "line\nbreak"
        assert_eq!(kinds("\"line\\nbreak\""), vec![Tok::Str("line\nbreak".into()), Tok::Eof]);
    }

    #[test]
    fn single_quote_escape_inside_single_string() {
        // source: 'it\'s'
        assert_eq!(kinds("'it\\'s'"), vec![Tok::Str("it's".into()), Tok::Eof]);
    }

    #[test]
    fn tab_escape_in_double_string() {
        // source: "tab\there"
        assert_eq!(kinds("\"tab\\there\""), vec![Tok::Str("tab\there".into()), Tok::Eof]);
    }

    #[test]
    fn backslash_escape_in_double_string() {
        // source: "back\\slash"
        assert_eq!(kinds("\"back\\\\slash\""), vec![Tok::Str("back\\slash".into()), Tok::Eof]);
    }

    #[test]
    fn rejects_unterminated_single_quoted_string() {
        let err = lex("'oops").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(kinds("1 // ignored\n+ 2"),
            vec![Tok::Number(1.0), Tok::Plus, Tok::Number(2.0), Tok::Eof]);
        // line comment to EOF (no trailing newline) is fine
        assert_eq!(kinds("42 // trailing"), vec![Tok::Number(42.0), Tok::Eof]);
    }

    #[test]
    fn skips_block_comments() {
        assert_eq!(kinds("1 /* a * b / c */ + 2"),
            vec![Tok::Number(1.0), Tok::Plus, Tok::Number(2.0), Tok::Eof]);
        // block comment spanning constructs
        assert_eq!(kinds("/* x */ 7"), vec![Tok::Number(7.0), Tok::Eof]);
    }

    #[test]
    fn division_is_not_a_comment() {
        assert_eq!(kinds("a / b"),
            vec![Tok::Ident("a".into()), Tok::Slash, Tok::Ident("b".into()), Tok::Eof]);
        assert_eq!(kinds("a /= b"),
            vec![Tok::Ident("a".into()), Tok::SlashEq, Tok::Ident("b".into()), Tok::Eof]);
    }

    #[test]
    fn unterminated_block_comment_errors() {
        let err = lex("/* oops no end").unwrap_err();
        assert!(err.message.contains("unterminated block comment"));
    }

    #[test]
    fn lexes_hex_literals() {
        assert_eq!(kinds("0xFF"), vec![Tok::Number(255.0), Tok::Eof]);
        assert_eq!(kinds("0xFF_FF"), vec![Tok::Number(65535.0), Tok::Eof]);
    }

    #[test]
    fn lexes_binary_literals() {
        assert_eq!(kinds("0b1010"), vec![Tok::Number(10.0), Tok::Eof]);
    }

    #[test]
    fn lexes_scientific_literals() {
        assert_eq!(kinds("1e9"), vec![Tok::Number(1e9), Tok::Eof]);
        assert_eq!(kinds("1.5e-3"), vec![Tok::Number(0.0015), Tok::Eof]);
    }

    #[test]
    fn lexes_underscore_separators() {
        assert_eq!(kinds("1_000"), vec![Tok::Number(1000.0), Tok::Eof]);
    }

    #[test]
    fn lexes_plain_decimals_and_floats() {
        assert_eq!(kinds("255"), vec![Tok::Number(255.0), Tok::Eof]);
        assert_eq!(kinds("2.5"), vec![Tok::Number(2.5), Tok::Eof]);
    }

    #[test]
    fn range_operator_not_consumed_as_float() {
        // `0..5` must lex as Number(0), DotDot, Number(5) — not `0.` float.
        assert_eq!(
            kinds("0..5"),
            vec![Tok::Number(0.0), Tok::DotDot, Tok::Number(5.0), Tok::Eof]
        );
    }

    #[test]
    fn member_access_after_ident_unaffected() {
        assert_eq!(
            kinds("a.b"),
            vec![Tok::Ident("a".into()), Tok::Dot, Tok::Ident("b".into()), Tok::Eof]
        );
    }

    #[test]
    fn invalid_hex_literal_errors() {
        let err = lex("0xZZ").unwrap_err();
        assert!(err.message.contains("invalid hex number literal"));
    }

    #[test]
    fn invalid_binary_literal_errors() {
        let err = lex("0b2").unwrap_err();
        assert!(err.message.contains("invalid binary number literal"));
    }
}
