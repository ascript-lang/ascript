//! Token kinds and tokens (a kind plus its source span).

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Number(f64),
    Str(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
    True,
    False,
    Nil,
    StarStar,
    Bang,
    BangEq,
    EqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AmpAmp,
    PipePipe,
    QuestionQuestion,
    Eq,
    Semicolon,
    Let,
    Const,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    LBrace,
    RBrace,
    If,
    Else,
    While,
    DotDot,
    /// `..=` — inclusive range, used only in match range patterns (Phase 8a).
    DotDotEq,
    DotDotDot,
    For,
    In,
    Return,
    Break,
    Continue,
    Fn,
    FatArrow,
    LBracket,
    RBracket,
    Dot,
    Colon,
    QuestionDot,
    Of,
    Instanceof,
    TemplateStr(String),    // a complete template with no interpolation: `...`
    TemplateStart(String),  // `...${   — text before the first interpolation
    TemplateMiddle(String), // }...${    — text between interpolations
    TemplateEnd(String),    // }...`     — text after the last interpolation
    Question,
    Pipe,
    Enum,
    Match,
    Class,
    Import,
    Export,
    Async,
    Await,
    Yield,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// A string is "identifier-like" if it can be a bare binding name / unquoted
/// object key: starts with a letter or `_`, followed by alphanumerics/`_`.
/// Shared by the parser (object-key keys) and the formatter (object_key emission).
pub(crate) fn is_ident_like(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_alphanumeric() || c == '_')
}
