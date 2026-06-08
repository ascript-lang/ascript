//! Hand-written lexer. Produces tokens with char-offset spans.

use crate::error::AsError;
use crate::lex_literals::{escape_char, parse_number_text, NumLit};
use crate::span::Span;
use crate::token::{Tok, Token};

/// Map a parsed numeric-literal subtype to its token (NUM §3.1).
fn num_lit_token(lit: NumLit) -> Tok {
    match lit {
        NumLit::Int(i) => Tok::Int(i),
        NumLit::Float(f) => Tok::Float(f),
    }
}

/// Lexer error message raised when a quoted string scan runs off the end of
/// input. Shared with `repl::is_unterminated_at_eof` so the message and the
/// "keep buffering" check have a single source of truth.
pub const ERR_UNTERMINATED_STRING: &str = "unterminated string";
/// Lexer error message raised when a template scan runs off the end of input.
/// Shared with `repl::is_unterminated_at_eof` (see `ERR_UNTERMINATED_STRING`).
pub const ERR_UNTERMINATED_TEMPLATE: &str = "unterminated template string";

enum TemplateChunk {
    Full,  // `...`           (no interpolation)
    Start, // `...${          (more follows)
}

/// Read a quoted string body starting just after the opening `quote`, scanning
/// until the matching unescaped `quote`. Advances `i` past the closing quote.
/// Backslash escapes are translated via `escape_char`.
fn lex_quoted(chars: &[char], i: &mut usize, start: usize, quote: char) -> Result<String, AsError> {
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
    Err(AsError::at(ERR_UNTERMINATED_STRING, Span::new(start, *i)))
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
    Err(AsError::at(ERR_UNTERMINATED_TEMPLATE, Span::new(start, *i)))
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
                    tokens.push(Token {
                        tok: Tok::PlusEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '%' {
                    // `+%` — wrapping add (NUM §3.2).
                    tokens.push(Token {
                        tok: Tok::PlusPercent,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Plus,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '-' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::MinusEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '%' {
                    // `-%` — wrapping subtract (NUM §3.2).
                    tokens.push(Token {
                        tok: Tok::MinusPercent,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Minus,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    tokens.push(Token {
                        tok: Tok::StarStar,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::StarEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '%' {
                    // `*%` — wrapping multiply (NUM §3.2).
                    tokens.push(Token {
                        tok: Tok::StarPercent,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Star,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::BangEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Bang,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::EqEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token {
                        tok: Tok::FatArrow,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Eq,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::Le,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '<' {
                    // `<<` — left shift (NUM §3.2). Longest-match before `<`.
                    tokens.push(Token {
                        tok: Tok::Shl,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Lt,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token {
                        tok: Tok::Ge,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    // `>>` — right shift (NUM §3.2). The lexer always emits a single
                    // `Shr`; the TYPE parser splits a trailing `>>` into two closing
                    // `>` (the Rust/Java/C# nested-generics technique).
                    tokens.push(Token {
                        tok: Tok::Shr,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Gt,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token {
                        tok: Tok::AmpAmp,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    // `&` — bitwise AND (NUM §3.2). `&&` matched above (longest-match).
                    tokens.push(Token {
                        tok: Tok::Amp,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '^' => {
                // `^` — bitwise XOR (NUM §3.2).
                tokens.push(Token {
                    tok: Tok::Caret,
                    span: Span::new(start, start + 1),
                });
                i += 1;
            }
            '~' => {
                // `~` — unary bitwise NOT (NUM §3.2).
                tokens.push(Token {
                    tok: Tok::Tilde,
                    span: Span::new(start, start + 1),
                });
                i += 1;
            }
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token {
                        tok: Tok::PipePipe,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Pipe,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token {
                        tok: Tok::QuestionQuestion,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token {
                        tok: Tok::QuestionDot,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Question,
                        span: Span::new(start, start + 1),
                    });
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
                    tokens.push(Token {
                        tok: Tok::SlashEq,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Slash,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '.' => {
                if i + 2 < chars.len() && chars[i + 1] == '.' && chars[i + 2] == '.' {
                    tokens.push(Token {
                        tok: Tok::DotDotDot,
                        span: Span::new(start, start + 3),
                    });
                    i += 3;
                } else if i + 2 < chars.len() && chars[i + 1] == '.' && chars[i + 2] == '=' {
                    // `..=` — inclusive range (used in match range patterns).
                    tokens.push(Token {
                        tok: Tok::DotDotEq,
                        span: Span::new(start, start + 3),
                    });
                    i += 3;
                } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token {
                        tok: Tok::DotDot,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        tok: Tok::Dot,
                        span: Span::new(start, start + 1),
                    });
                    i += 1;
                }
            }
            '%' => push(&mut tokens, Tok::Percent, start, &mut i),
            '(' => push(&mut tokens, Tok::LParen, start, &mut i),
            ')' => push(&mut tokens, Tok::RParen, start, &mut i),
            ',' => push(&mut tokens, Tok::Comma, start, &mut i),
            ';' => push(&mut tokens, Tok::Semicolon, start, &mut i),
            ':' => push(&mut tokens, Tok::Colon, start, &mut i),
            '#' => {
                if i + 1 < chars.len() && chars[i + 1] == '{' {
                    // `#{` opens a map literal — one token, and it opens a
                    // brace context (closed by `}`), so track the depth.
                    brace_depth += 1;
                    tokens.push(Token {
                        tok: Tok::HashBrace,
                        span: Span::new(start, start + 2),
                    });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '#'",
                        Span::new(start, start + 1),
                    ));
                }
            }
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
                tokens.push(Token {
                    tok: Tok::Str(s),
                    span: Span::new(start, i),
                });
            }
            '\'' => {
                i += 1;
                let s = lex_quoted(&chars, &mut i, start, '\'')?;
                tokens.push(Token {
                    tok: Tok::Str(s),
                    span: Span::new(start, i),
                });
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
                    && matches!(chars[i + 1], 'x' | 'X' | 'b' | 'B' | 'o' | 'O')
                {
                    let radix_char = chars[i + 1];
                    let is_digit: fn(char) -> bool = match radix_char {
                        'x' | 'X' => |d| d.is_ascii_hexdigit(),
                        'o' | 'O' => |d| ('0'..='7').contains(&d),
                        _ => |d| d == '0' || d == '1',
                    };
                    j = i + 2;
                    while j < chars.len() && (is_digit(chars[j]) || chars[j] == '_') {
                        j += 1;
                    }
                    let span = Span::new(i, j);
                    let label = match radix_char {
                        'x' | 'X' => "invalid hex number literal",
                        'o' | 'O' => "invalid octal number literal",
                        _ => "invalid binary number literal",
                    };
                    // Pass the full token text (incl. `0x`/`0b`/`0o` prefix) to the
                    // shared parser; it strips underscores and dispatches on the
                    // prefix. Radix literals are always `int`; an empty body is
                    // `Invalid`, an i64 overflow is `OutOfRange` (NUM §3.1).
                    let text: String = chars[i..j].iter().collect();
                    let lit = parse_number_text(&text)
                        .map_err(|e| AsError::at(e.message(label), span))?;
                    tokens.push(Token {
                        tok: num_lit_token(lit),
                        span,
                    });
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
                    let text: String = chars[i..j].iter().collect();
                    let lit = parse_number_text(&text)
                        .map_err(|e| AsError::at(e.message("invalid number"), span))?;
                    tokens.push(Token {
                        tok: num_lit_token(lit),
                        span,
                    });
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
                    "instanceof" => Tok::Instanceof,
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
                    "yield" => Tok::Yield,
                    _ => Tok::Ident(text),
                };
                tokens.push(Token {
                    tok,
                    span: Span::new(i, j),
                });
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

    tokens.push(Token {
        tok: Tok::Eof,
        span: Span::new(chars.len(), chars.len()),
    });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, tok: Tok, start: usize, i: &mut usize) {
    tokens.push(Token {
        tok,
        span: Span::new(start, start + 1),
    });
    *i += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn step_is_a_plain_identifier() {
        // `step` is a contextual keyword recognized in the parser, NOT reserved
        // in the lexer — so `let step = 1` must keep `step` as a plain Ident.
        let toks = lex("let step = 1").unwrap();
        assert!(
            matches!(toks[1].tok, Tok::Ident(ref s) if s == "step"),
            "expected Ident(\"step\"), got {:?}",
            toks[1].tok
        );
    }

    #[test]
    fn lexes_question_variants() {
        assert_eq!(
            kinds("a ? b ?? c?.d"),
            vec![
                Tok::Ident("a".into()),
                Tok::Question,
                Tok::Ident("b".into()),
                Tok::QuestionQuestion,
                Tok::Ident("c".into()),
                Tok::QuestionDot,
                Tok::Ident("d".into()),
                Tok::Eof,
            ]
        );
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
                Tok::Int(1),
                Tok::Plus,
                Tok::Int(2),
                Tok::Star,
                Tok::Int(3),
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
                Tok::Ident("a".into()),
                Tok::StarStar,
                Tok::Ident("b".into()),
                Tok::EqEq,
                Tok::Ident("c".into()),
                Tok::BangEq,
                Tok::Ident("d".into()),
                Tok::Le,
                Tok::Ident("e".into()),
                Tok::Ge,
                Tok::Ident("f".into()),
                Tok::AmpAmp,
                Tok::Ident("g".into()),
                Tok::PipePipe,
                Tok::Ident("h".into()),
                Tok::QuestionQuestion,
                Tok::Ident("i".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_single_char_comparisons_and_bang() {
        assert_eq!(
            kinds("!a < b > c"),
            vec![
                Tok::Bang,
                Tok::Ident("a".into()),
                Tok::Lt,
                Tok::Ident("b".into()),
                Tok::Gt,
                Tok::Ident("c".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_bitwise_and_wrapping_operators() {
        // NUM §3.2: the new operator tokens, with longest-match disambiguation.
        assert_eq!(
            kinds("a & b ^ c ~ d << e >> f +% g -% h *% i"),
            vec![
                Tok::Ident("a".into()),
                Tok::Amp,
                Tok::Ident("b".into()),
                Tok::Caret,
                Tok::Ident("c".into()),
                Tok::Tilde,
                Tok::Ident("d".into()),
                Tok::Shl,
                Tok::Ident("e".into()),
                Tok::Shr,
                Tok::Ident("f".into()),
                Tok::PlusPercent,
                Tok::Ident("g".into()),
                Tok::MinusPercent,
                Tok::Ident("h".into()),
                Tok::StarPercent,
                Tok::Ident("i".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn longest_match_keeps_logical_and_bitwise_distinct() {
        // `&&`/`||` stay the logical tokens; a lone `&`/`|` are bitwise. `<<`/`>>`
        // win over `<`/`>`; `+%`/`*%` win over `+`/`*`.
        assert_eq!(kinds("&&"), vec![Tok::AmpAmp, Tok::Eof]);
        assert_eq!(kinds("&"), vec![Tok::Amp, Tok::Eof]);
        assert_eq!(kinds("||"), vec![Tok::PipePipe, Tok::Eof]);
        assert_eq!(kinds("|"), vec![Tok::Pipe, Tok::Eof]);
        assert_eq!(kinds("<<"), vec![Tok::Shl, Tok::Eof]);
        assert_eq!(kinds("<"), vec![Tok::Lt, Tok::Eof]);
        assert_eq!(kinds(">>"), vec![Tok::Shr, Tok::Eof]);
        assert_eq!(kinds("+%"), vec![Tok::PlusPercent, Tok::Eof]);
        assert_eq!(kinds("+"), vec![Tok::Plus, Tok::Eof]);
        // `&` followed by `&` (with no space) is still `&&`, not two `&`.
        assert_eq!(
            kinds("a&&b"),
            vec![
                Tok::Ident("a".into()),
                Tok::AmpAmp,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
        // `a&b` (no space) is bitwise-AND.
        assert_eq!(
            kinds("a&b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Amp,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn lexes_compound_assignment_operators() {
        assert_eq!(
            kinds("a += b -= c *= d /= e"),
            vec![
                Tok::Ident("a".into()),
                Tok::PlusEq,
                Tok::Ident("b".into()),
                Tok::MinusEq,
                Tok::Ident("c".into()),
                Tok::StarEq,
                Tok::Ident("d".into()),
                Tok::SlashEq,
                Tok::Ident("e".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_fat_arrow() {
        assert_eq!(
            kinds("x => x"),
            vec![
                Tok::Ident("x".into()),
                Tok::FatArrow,
                Tok::Ident("x".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn lexes_plain_template() {
        assert_eq!(
            kinds("`hello`"),
            vec![Tok::TemplateStr("hello".into()), Tok::Eof]
        );
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
    fn template_interpolation_contains_string_literal() {
        // A string literal inside `${...}` lexes as an ordinary Str token; the
        // closing `}` of the interpolation is still found correctly.
        assert_eq!(
            kinds("`x${\"hi\"}y`"),
            vec![
                Tok::TemplateStart("x".into()),
                Tok::Str("hi".into()),
                Tok::TemplateEnd("y".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn template_interpolation_string_with_braces_and_dollar() {
        // Braces and `${` *inside* the nested string are part of the string and
        // must NOT be treated as interpolation delimiters — string-lexing wins.
        assert_eq!(
            kinds("`${\"a}b{c ${d}\"}`"),
            vec![
                Tok::TemplateStart("".into()),
                Tok::Str("a}b{c ${d}".into()),
                Tok::TemplateEnd("".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn template_interpolation_nested_template() {
        // A template nested inside another template's interpolation:
        // `${`hi ${n}`}`
        assert_eq!(
            kinds("`${`hi ${n}`}`"),
            vec![
                Tok::TemplateStart("".into()),
                Tok::TemplateStart("hi ".into()),
                Tok::Ident("n".into()),
                Tok::TemplateEnd("".into()),
                Tok::TemplateEnd("".into()),
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
        assert_eq!(
            kinds("\"line\\nbreak\""),
            vec![Tok::Str("line\nbreak".into()), Tok::Eof]
        );
    }

    #[test]
    fn single_quote_escape_inside_single_string() {
        // source: 'it\'s'
        assert_eq!(kinds("'it\\'s'"), vec![Tok::Str("it's".into()), Tok::Eof]);
    }

    #[test]
    fn tab_escape_in_double_string() {
        // source: "tab\there"
        assert_eq!(
            kinds("\"tab\\there\""),
            vec![Tok::Str("tab\there".into()), Tok::Eof]
        );
    }

    #[test]
    fn backslash_escape_in_double_string() {
        // source: "back\\slash"
        assert_eq!(
            kinds("\"back\\\\slash\""),
            vec![Tok::Str("back\\slash".into()), Tok::Eof]
        );
    }

    #[test]
    fn rejects_unterminated_single_quoted_string() {
        let err = lex("'oops").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(
            kinds("1 // ignored\n+ 2"),
            vec![Tok::Int(1), Tok::Plus, Tok::Int(2), Tok::Eof]
        );
        // line comment to EOF (no trailing newline) is fine
        assert_eq!(kinds("42 // trailing"), vec![Tok::Int(42), Tok::Eof]);
    }

    #[test]
    fn skips_block_comments() {
        assert_eq!(
            kinds("1 /* a * b / c */ + 2"),
            vec![Tok::Int(1), Tok::Plus, Tok::Int(2), Tok::Eof]
        );
        // block comment spanning constructs
        assert_eq!(kinds("/* x */ 7"), vec![Tok::Int(7), Tok::Eof]);
    }

    #[test]
    fn division_is_not_a_comment() {
        assert_eq!(
            kinds("a / b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Slash,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
        assert_eq!(
            kinds("a /= b"),
            vec![
                Tok::Ident("a".into()),
                Tok::SlashEq,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn unterminated_block_comment_errors() {
        let err = lex("/* oops no end").unwrap_err();
        assert!(err.message.contains("unterminated block comment"));
    }

    #[test]
    fn lexes_hex_literals() {
        assert_eq!(kinds("0xFF"), vec![Tok::Int(255), Tok::Eof]);
        assert_eq!(kinds("0xFF_FF"), vec![Tok::Int(65535), Tok::Eof]);
    }

    #[test]
    fn lexes_binary_literals() {
        assert_eq!(kinds("0b1010"), vec![Tok::Int(10), Tok::Eof]);
    }

    #[test]
    fn lexes_octal_literals() {
        assert_eq!(kinds("0o17"), vec![Tok::Int(15), Tok::Eof]);
        assert_eq!(kinds("0O17"), vec![Tok::Int(15), Tok::Eof]);
    }

    #[test]
    fn lexes_scientific_literals() {
        // An exponent makes the literal a float even when integral.
        assert_eq!(kinds("1e9"), vec![Tok::Float(1e9), Tok::Eof]);
        assert_eq!(kinds("1.5e-3"), vec![Tok::Float(0.0015), Tok::Eof]);
    }

    #[test]
    fn lexes_underscore_separators() {
        assert_eq!(kinds("1_000"), vec![Tok::Int(1000), Tok::Eof]);
    }

    #[test]
    fn lexes_plain_decimals_and_floats() {
        assert_eq!(kinds("255"), vec![Tok::Int(255), Tok::Eof]);
        assert_eq!(kinds("2.5"), vec![Tok::Float(2.5), Tok::Eof]);
    }

    #[test]
    fn integer_literal_overflow_is_a_lex_error() {
        let err = lex("9223372036854775808").unwrap_err();
        assert!(
            err.message
                .contains("integer literal out of range for int (i64)"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn range_operator_not_consumed_as_float() {
        // `0..5` must lex as Int(0), DotDot, Int(5) — not `0.` float.
        assert_eq!(
            kinds("0..5"),
            vec![Tok::Int(0), Tok::DotDot, Tok::Int(5), Tok::Eof]
        );
    }

    #[test]
    fn lexes_triple_dot_as_spread_not_two_dots() {
        let toks = lex("...x").unwrap();
        assert_eq!(toks[0].tok, Tok::DotDotDot);
        assert!(matches!(toks[1].tok, Tok::Ident(ref s) if s == "x"));
        let r = lex("0..5").unwrap();
        assert_eq!(r[1].tok, Tok::DotDot);
    }

    #[test]
    fn member_access_after_ident_unaffected() {
        assert_eq!(
            kinds("a.b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Dot,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
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
