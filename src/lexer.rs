//! Hand-written lexer. Produces tokens with char-offset spans.

use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

pub fn lex(src: &str) -> Result<Vec<Token>, AsError> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

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
                    return Err(AsError::at("unexpected character '|'", Span::new(start, start + 1)));
                }
            }
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token { tok: Tok::QuestionQuestion, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '?' (the ?. and ? operators arrive in Milestone 3)",
                        Span::new(start, start + 1),
                    ));
                }
            }
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::SlashEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Slash, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '%' => push(&mut tokens, Tok::Percent, start, &mut i),
            '(' => push(&mut tokens, Tok::LParen, start, &mut i),
            ')' => push(&mut tokens, Tok::RParen, start, &mut i),
            ',' => push(&mut tokens, Tok::Comma, start, &mut i),
            ';' => push(&mut tokens, Tok::Semicolon, start, &mut i),
            '{' => push(&mut tokens, Tok::LBrace, start, &mut i),
            '}' => push(&mut tokens, Tok::RBrace, start, &mut i),
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
            c if c.is_ascii_digit() => {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '.' {
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
    fn rejects_unterminated_string() {
        let err = lex("\"oops").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }
}
