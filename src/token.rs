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
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}
