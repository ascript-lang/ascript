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
    /// The OUTER/context source (the entry module's), used as a fallback when no
    /// span-bound source is set. `with_source` is innermost-wins.
    pub source: Option<Rc<SourceInfo>>,
    /// The source the `span`'s byte offsets index, bound at the moment the error
    /// is RAISED (in the module whose AST/chunk the span belongs to). A span is
    /// only meaningful paired with the source it indexes, so the renderer prefers
    /// this over `source` for the caret — fixing cross-module provenance where a
    /// panic raised in module A but propagating up to B's call site would
    /// otherwise render A's span against B's text (SP4 §3). `None` for a
    /// single-module error (the renderer falls back to `source`, unchanged).
    pub span_source: Option<Rc<SourceInfo>>,
}

impl AsError {
    pub fn new(message: impl Into<String>) -> Self {
        AsError {
            message: message.into(),
            span: None,
            source: None,
            span_source: None,
        }
    }

    pub fn at(message: impl Into<String>, span: Span) -> Self {
        AsError {
            message: message.into(),
            span: Some(span),
            source: None,
            span_source: None,
        }
    }

    /// Like [`at`], but ALSO binds the `span`'s own source at raise time, so the
    /// span and the text it indexes can never be paired with a different module's
    /// source on the way up the call stack (SP4 §3 cross-module provenance).
    pub fn at_in(message: impl Into<String>, span: Span, src: Rc<SourceInfo>) -> Self {
        AsError {
            message: message.into(),
            span: Some(span),
            source: None,
            span_source: Some(src),
        }
    }

    /// Attach OUTER/context source context, but only if none is already set so the
    /// innermost module's source wins. Does NOT touch `span_source` (the
    /// span-bound source, which is set at raise time and is authoritative for the
    /// caret).
    pub fn with_source(mut self, src: Rc<SourceInfo>) -> Self {
        if self.source.is_none() {
            self.source = Some(src);
        }
        self
    }

    /// Bind the span's own source, but only if not already bound (innermost-wins,
    /// mirroring `with_source`). Used to attach the executing module's source at
    /// the frame that owns the span before the error crosses an import boundary.
    pub fn with_span_source(mut self, src: Rc<SourceInfo>) -> Self {
        if self.span_source.is_none() {
            self.span_source = Some(src);
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
