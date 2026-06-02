//! Parser output is a flat list of events, not a tree built directly. This
//! decouples grammar decisions (which ignore trivia) from tree construction
//! (which re-inserts trivia). `Start` carries a `forward_parent` slot so a
//! completed node can be retro-actively wrapped by an outer node (needed for
//! left-associative binary expressions discovered after parsing the lhs).

use crate::syntax::kind::SyntaxKind;

#[derive(Debug, Clone)]
pub enum Event {
    /// Open a node. `kind` is `TOMBSTONE` until the node is completed; some
    /// Start events are abandoned (left as Tombstone) and skipped by the builder.
    Start { kind: SyntaxKind, forward_parent: Option<usize> },
    /// Finish the current node.
    Finish,
    /// Consume the next non-trivia token (the builder pulls the actual token,
    /// including any preceding trivia, from the token stream).
    Token { kind: SyntaxKind },
    /// A parse error at the current position; carries a message for diagnostics.
    Error { message: String },
}

/// Placeholder kind for a Start event not yet assigned a node kind, or abandoned.
/// The builder skips Tombstone Start events.
pub const TOMBSTONE: SyntaxKind = SyntaxKind::Error;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_constructible() {
        let evs = [
            Event::Start { kind: SyntaxKind::SourceFile, forward_parent: None },
            Event::Token { kind: SyntaxKind::Number },
            Event::Finish,
        ];
        assert_eq!(evs.len(), 3);
    }
}
