//! Hand-written recursive-descent parser. Operates over the NON-trivia tokens
//! (trivia is skipped for grammar decisions and re-inserted by the tree builder)
//! and emits a `Vec<Event>` plus a list of `ParseError`s. Never aborts: on error
//! it emits an `Error` event and recovers, so it always yields a tree.

use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::{lex, LexToken};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Index into the *non-trivia* token list where the error occurred.
    pub token_index: usize,
}

pub struct Parse {
    pub events: Vec<Event>,
    pub errors: Vec<ParseError>,
    /// The full token stream (incl. trivia), needed by the tree builder.
    pub tokens: Vec<LexToken>,
}

struct Parser {
    tokens: Vec<LexToken>,
    /// Indices (into `tokens`) of the non-trivia tokens, in order.
    nontrivia: Vec<usize>,
    /// Cursor into `nontrivia`.
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

/// A pending open node. `complete` sets its kind; if dropped uncompleted it
/// stays a Tombstone and is skipped.
struct Marker {
    pos: usize, // index into events of the Start
    completed: bool,
}

struct CompletedMarker {
    pos: usize, // index into events of the Start
}

impl Parser {
    fn new(src: &str) -> Self {
        let tokens = lex(src);
        let nontrivia = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i)
            .collect();
        Parser { tokens, nontrivia, pos: 0, events: Vec::new(), errors: Vec::new() }
    }

    fn current(&self) -> SyntaxKind {
        match self.nontrivia.get(self.pos) {
            Some(&ti) => self.tokens[ti].kind,
            None => SyntaxKind::Error,
        }
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn at_end(&self) -> bool {
        self.pos >= self.nontrivia.len()
    }

    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        Marker { pos, completed: false }
    }

    fn bump(&mut self) {
        let kind = self.current();
        if !self.at_end() {
            self.events.push(Event::Token { kind });
            self.pos += 1;
        }
    }

    fn complete(&mut self, mut m: Marker, kind: SyntaxKind) -> CompletedMarker {
        m.completed = true;
        if let Event::Start { kind: slot, .. } = &mut self.events[m.pos] {
            *slot = kind;
        }
        self.events.push(Event::Finish);
        CompletedMarker { pos: m.pos }
    }

    // used in Plan 2 T6+
    #[allow(dead_code)]
    fn precede(&mut self, cm: &CompletedMarker) -> Marker {
        let new_pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        if let Event::Start { forward_parent, .. } = &mut self.events[cm.pos] {
            *forward_parent = Some(new_pos);
        }
        Marker { pos: new_pos, completed: false }
    }

    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.errors.push(ParseError { message: message.clone(), token_index: self.pos });
        self.events.push(Event::Error { message });
    }
}

/// Parse `src` into events + errors + the token stream.
pub fn parse(src: &str) -> Parse {
    let mut p = Parser::new(src);
    let m = p.start();
    while !p.at_end() {
        let before = p.pos;
        stmt(&mut p);
        if p.pos == before {
            p.error("unexpected token");
            p.bump();
        }
    }
    p.complete(m, SyntaxKind::SourceFile);
    Parse { events: p.events, errors: p.errors, tokens: p.tokens }
}

/// Placeholder statement parser — later tasks fill this in. For T3 it parses a
/// single number literal as an expression statement so the scaffold is testable.
fn stmt(p: &mut Parser) {
    let m = p.start();
    if p.at(SyntaxKind::Number) {
        let lit = p.start();
        p.bump();
        p.complete(lit, SyntaxKind::Literal);
        p.complete(m, SyntaxKind::ExprStmt);
    } else {
        p.error("expected statement");
        p.bump();
        p.complete(m, SyntaxKind::Error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_kinds(src: &str) -> Vec<SyntaxKind> {
        parse(src)
            .events
            .into_iter()
            .filter_map(|e| match e {
                Event::Start { kind, .. } if kind != crate::syntax::event::TOMBSTONE => Some(kind),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_a_number_statement() {
        assert_eq!(
            node_kinds("42"),
            vec![SyntaxKind::SourceFile, SyntaxKind::ExprStmt, SyntaxKind::Literal]
        );
        assert!(parse("42").errors.is_empty());
    }

    #[test]
    fn unexpected_token_recovers_not_panics() {
        let p = parse("+");
        assert!(!p.errors.is_empty(), "should record an error");
        assert!(matches!(p.events.first(), Some(Event::Start { kind: SyntaxKind::SourceFile, .. })));
    }
}
