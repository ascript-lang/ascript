//! Unified error type for every stage (lex, parse, eval).

use crate::span::Span;
use std::fmt;

#[derive(Debug)]
pub struct AsError {
    pub message: String,
    pub span: Option<Span>,
}

impl AsError {
    pub fn new(message: impl Into<String>) -> Self {
        AsError { message: message.into(), span: None }
    }

    pub fn at(message: impl Into<String>, span: Span) -> Self {
        AsError { message: message.into(), span: Some(span) }
    }
}

impl fmt::Display for AsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(s) => write!(f, "{} (at {}..{})", self.message, s.start, s.end),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for AsError {}
