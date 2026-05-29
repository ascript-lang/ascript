//! Hand-written lexer. Produces tokens with char-offset spans.

use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

enum TemplateChunk {
    Full,  // `...`           (no interpolation)
    Start, // `...${          (more follows)
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
            // simple escapes inside templates: \` \$ \\ \n \t
            *i += 1;
            let e = chars[*i];
            text.push(match e {
                'n' => '\n',
                't' => '\t',
                other => other,
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
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(AsError::at("unterminated string", Span::new(start, i)));
                }
                i += 1; // consume closing quote
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
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j + 1 < chars.len() && chars[j] == '.' && chars[j + 1].is_ascii_digit() {
                    j += 1;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                let text: String = chars[i..j].iter().collect();
                let n: f64 = text
                    .parse()
                    .map_err(|_| AsError::at("invalid number", Span::new(i, j)))?;
                tokens.push(Token { tok: Tok::Number(n), span: Span::new(i, j) });
                i = j;
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
}
