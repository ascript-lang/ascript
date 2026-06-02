//! Name resolution over the typed CST. See types.rs for the produced data.

pub mod types;

use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use cstree::text::TextRange;
use types::*;

/// Resolve a parsed source file.
pub fn resolve(root: &ResolvedNode) -> ResolveResult {
    let mut r = Resolver::new();
    r.resolve_file(root);
    r.finish()
}

/// First IDENT token's text within `node` (via the resolver-backed tree).
pub fn ident_text(node: &ResolvedNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

/// Stable map key for a use site: its text range.
pub fn use_key(node: &ResolvedNode) -> TextRange {
    node.text_range()
}

struct Resolver {
    result: ResolveResult,
}

impl Resolver {
    fn new() -> Self {
        Resolver {
            result: ResolveResult::default(),
        }
    }

    fn resolve_file(&mut self, _root: &ResolvedNode) {
        // Filled in across Tasks 2–7.
    }

    fn finish(self) -> ResolveResult {
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn res(src: &str) -> ResolveResult {
        resolve(&parse_to_tree(src))
    }

    #[test]
    fn empty_program_resolves() {
        let r = res("");
        assert!(r.uses.is_empty());
        assert!(r.diagnostics.is_empty());
    }
}
