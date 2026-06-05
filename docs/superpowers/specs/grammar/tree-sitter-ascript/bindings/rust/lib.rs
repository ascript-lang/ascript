//! This crate provides AScript language support for the [tree-sitter] parsing library.
//!
//! [tree-sitter]: https://tree-sitter.github.io/

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_ascript() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for this grammar.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_ascript) };

/// The syntax-highlighting query for this grammar.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// The language-injection query for this grammar.
pub const INJECTIONS_QUERY: &str = include_str!("../../queries/injections.scm");

/// The local-variable (scopes/definitions/references) query for this grammar.
pub const LOCALS_QUERY: &str = include_str!("../../queries/locals.scm");

/// The code-folding query for this grammar.
pub const FOLDS_QUERY: &str = include_str!("../../queries/folds.scm");
// NOTE: INDENTS/TEXTOBJECTS/TAGS/BRACKETS query constants are
// added by their creating tasks (Phase 5 Tasks 7–10) so this crate always compiles —
// `include_str!` requires the target file to exist.

#[cfg(test)]
mod tests {
    #[test]
    fn test_can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading AScript parser");
    }
}
