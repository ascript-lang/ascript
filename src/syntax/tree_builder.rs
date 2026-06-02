//! Materialize a cstree green tree from parser events, re-inserting trivia from
//! the original token stream so the tree is byte-for-byte lossless.
//!
//! Trivia attachment policy: trivia is emitted at the point in the token stream
//! where it occurs. Before emitting each non-trivia Token, the builder first
//! flushes any trivia tokens that precede it. This guarantees losslessness;
//! node-relative leading/trailing attachment is a formatter concern (Plan 4).

use crate::syntax::cst::{ResolvedNode, SyntaxNode};
use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::LexToken;
use crate::syntax::parser::Parse;
use cstree::build::GreenNodeBuilder;

pub fn build_tree(parse: Parse) -> ResolvedNode {
    let Parse { mut events, tokens, .. } = parse;

    let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
    let mut token_pos = 0usize; // cursor into `tokens` (incl. trivia)

    fn flush_trivia(
        builder: &mut GreenNodeBuilder<SyntaxKind>,
        tokens: &[LexToken],
        token_pos: &mut usize,
    ) {
        while *token_pos < tokens.len() && tokens[*token_pos].kind.is_trivia() {
            let t = &tokens[*token_pos];
            builder.token(t.kind, &t.text);
            *token_pos += 1;
        }
    }

    let resolved = resolve_forward_parents(&mut events);

    // Track open-node depth so trailing trivia (after the last non-trivia token)
    // is flushed INSIDE the root before the root's finish_node.
    let mut depth: usize = 0;

    for ev in resolved {
        match ev {
            Event::Start { kind, .. } if kind != TOMBSTONE => {
                builder.start_node(kind);
                depth += 1;
            }
            Event::Start { .. } => { /* tombstone: skip */ }
            Event::Finish => {
                depth -= 1;
                if depth == 0 {
                    flush_trivia(&mut builder, &tokens, &mut token_pos);
                }
                builder.finish_node();
            }
            Event::Token { kind } => {
                flush_trivia(&mut builder, &tokens, &mut token_pos);
                debug_assert!(token_pos < tokens.len());
                let t = &tokens[token_pos];
                builder.token(kind, &t.text);
                token_pos += 1;
            }
            Event::Error { .. } => { /* errors don't materialize tokens */ }
        }
    }

    let (green, cache) = builder.finish();
    let resolver = cache.unwrap().into_interner().unwrap();
    SyntaxNode::new_root_with_resolver(green, resolver)
}

/// Reorder events so any node referenced as a `forward_parent` is started
/// immediately before the node that points to it (rowan/rust-analyzer's retro-
/// active node wrapping for left-assoc binary expressions).
fn resolve_forward_parents(events: &mut [Event]) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    for i in 0..events.len() {
        match events[i].clone() {
            Event::Start { kind, forward_parent } if kind != TOMBSTONE => {
                let mut chain = vec![kind];
                let mut fp = forward_parent;
                while let Some(idx) = fp {
                    if let Event::Start { kind: pk, forward_parent: pfp } = events[idx].clone() {
                        events[idx] = Event::Start { kind: TOMBSTONE, forward_parent: None };
                        chain.push(pk);
                        fp = pfp;
                    } else {
                        break;
                    }
                }
                for k in chain.into_iter().rev() {
                    out.push(Event::Start { kind: k, forward_parent: None });
                }
            }
            Event::Start { .. } => { /* consumed/tombstone: skip */ }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    #[test]
    fn structured_tree_round_trips() {
        let src = "  42 // trailing\n";
        let node = build_tree(parse(src));
        assert_eq!(node.text().to_string(), src, "structured tree must be lossless");
    }
}
