//! Unified error type for every stage (lex, parse, eval).

use crate::span::Span;
use std::fmt;
use std::rc::Rc;

/// Source context (file path + full text) attached to an error so multi-file
/// diagnostics can render a caret pointing into the right module.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct AsError {
    pub message: String,
    pub span: Option<Span>,
    pub source: Option<Rc<SourceInfo>>,
}

impl AsError {
    pub fn new(message: impl Into<String>) -> Self {
        AsError {
            message: message.into(),
            span: None,
            source: None,
        }
    }

    pub fn at(message: impl Into<String>, span: Span) -> Self {
        AsError {
            message: message.into(),
            span: Some(span),
            source: None,
        }
    }

    /// Attach source context, but only if none is already set so the innermost
    /// module's source wins.
    pub fn with_source(mut self, src: Rc<SourceInfo>) -> Self {
        if self.source.is_none() {
            self.source = Some(src);
        }
        self
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
