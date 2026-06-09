//! `field-default-type` (structural): flag a class field whose LITERAL default
//! provably contradicts its declared type — a guaranteed runtime panic at
//! construction (`type contract violated: expected <T>, got <kind>`).
//!
//! A `FieldDecl` is `name (?)?: <type> (= <default>)?`. We only act when BOTH a
//! type annotation and a *literal* default are present. The default's primitive
//! kind (number/string/bool/nil) is compared against the declared type using the
//! shared literal-vs-type compatibility logic in `check::rules` (the same engine
//! the `contract-mismatch` rule uses). `nil` is incompatible with a non-optional
//! type; a field marked optional (`name?: T` marker token OR an `OptionalType`
//! `T?`) accepts `nil`. Computed/non-literal defaults and generic/uncertain types
//! are skipped (conservative — only PROVABLE contradictions flag).

use crate::check::diagnostic::{AsDiagnostic, Severity};
use crate::check::rules::{
    code_range, is_expr_kind, is_type_kind, lit_name, literal_kind, type_compat, Compat, LitKind,
};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::ResolveResult;

pub fn check(tree: &ResolvedNode, _resolved: &ResolveResult, _src: &str) -> Vec<AsDiagnostic> {
    use SyntaxKind::*;

    let mut out = Vec::new();
    for field in tree.descendants().filter(|n| n.kind() == FieldDecl) {
        // The type annotation child node (the first type-kind child).
        let Some(ty) = field.children().find(|c| is_type_kind(c.kind())) else {
            continue; // unannotated — nothing to contradict
        };
        // The default value: the field's expression child (`= <default>`). The
        // type is a type-kind node, not an expr-kind node, so the first expr-kind
        // child is unambiguously the default.
        let Some(default) = field.children().find(|c| is_expr_kind(c.kind())) else {
            continue; // no default
        };
        let Some(lit) = literal_kind(default) else {
            continue; // computed / non-literal default — skip
        };

        // A field is optional (accepts nil) if it carries the `?` marker token
        // (`name?: T`). `type_compat` already handles `OptionalType` (`T?`).
        let optional_marker = field
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .any(|t| t.kind() == Question);
        if lit == LitKind::Nil && optional_marker {
            continue;
        }

        if type_compat(ty, lit) == Compat::No {
            let name = crate::syntax::resolve::ident_text(field).unwrap_or_default();
            out.push(AsDiagnostic {
                range: code_range(field),
                severity: Severity::Warning,
                code: "field-default-type".to_string(),
                message: format!(
                    "field '{name}' default is {}, which violates its declared type {}",
                    lit_name(lit),
                    ty.text().to_string().trim()
                ),
                fix: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;

    /// Count the `field-default-type` diagnostics the RULE emits directly. Since
    /// TYPE, the end-to-end `analyze` pipeline DROPS a `field-default-type` advisory
    /// at a span where the inference pass emits a blocking `type-mismatch` Error
    /// (subsumption — the user sees the sound Error, not a duplicate Warning). These
    /// unit tests exercise the rule's own detection, so they invoke it directly.
    fn count(src: &str, code: &str) -> usize {
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        super::check(&tree, &resolved, src)
            .iter()
            .filter(|d| d.code == code)
            .count()
    }

    #[test]
    fn number_field_string_default_flagged() {
        let src = "class P { n: number = \"x\" }";
        assert_eq!(
            count(src, "field-default-type"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn string_field_number_default_flagged() {
        let src = "class P { s: string = 5 }";
        assert_eq!(
            count(src, "field-default-type"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn matching_number_default_ok() {
        let src = "class P { n: number = 5 }";
        assert_eq!(
            count(src, "field-default-type"),
            0,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn matching_string_default_ok() {
        let src = "class P { s: string = \"ok\" }";
        assert_eq!(
            count(src, "field-default-type"),
            0,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn nil_default_to_optional_ok() {
        let src = "class P { n: number? = nil }";
        assert_eq!(
            count(src, "field-default-type"),
            0,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn nil_default_to_non_optional_flagged() {
        let src = "class P { n: number = nil }";
        assert_eq!(
            count(src, "field-default-type"),
            1,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn computed_default_skipped() {
        let src = "class P { xs: array<number> = foo() }";
        assert_eq!(
            count(src, "field-default-type"),
            0,
            "{:?}",
            analyze(src).diagnostics
        );
    }

    #[test]
    fn message_names_field_kind_and_type() {
        let src = "class P { n: number = \"x\" }";
        let tree = crate::syntax::tree_builder::build_tree(crate::syntax::parser::parse(src));
        let resolved = crate::syntax::resolve::resolve(&tree);
        let d = super::check(&tree, &resolved, src)
            .into_iter()
            .find(|d| d.code == "field-default-type")
            .unwrap();
        assert_eq!(
            d.message,
            "field 'n' default is string, which violates its declared type number"
        );
    }

    #[test]
    fn typed_field_default_surfaces_as_blocking_type_mismatch() {
        // End-to-end: through `analyze` the user sees a single BLOCKING
        // `type-mismatch` Error (the legacy `field-default-type` advisory is dropped
        // at this span — TYPE subsumption).
        let src = "class P { n: number = \"x\" }";
        let ds = analyze(src).diagnostics;
        assert_eq!(ds.iter().filter(|d| d.code == "field-default-type").count(), 0, "{ds:?}");
        let tm: Vec<_> = ds.iter().filter(|d| d.code == "type-mismatch").collect();
        assert_eq!(tm.len(), 1, "{ds:?}");
        assert_eq!(tm[0].severity, crate::check::diagnostic::Severity::Error);
    }
}
