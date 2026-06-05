//! `textDocument/foldingRange`, `selectionRange`, and `documentLink` — structural
//! providers over the cached CST (`model.tree`). No re-parse; pure
//! `fn(&SemanticModel, …)`.

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind};

/// Foldable ranges: every multi-line `Block` / class / enum / array / object /
/// match node, plus `//region` … `//endregion` line-comment pairs. Line-based
/// folds (start line .. end line).
pub fn folding_ranges(model: &SemanticModel) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    // 1. Structural folds from the CST.
    for node in model.tree.descendants() {
        let foldable = matches!(
            node.kind(),
            SyntaxKind::Block
                | SyntaxKind::ClassDecl
                | SyntaxKind::EnumDecl
                | SyntaxKind::ArrayExpr
                | SyntaxKind::ObjectExpr
                | SyntaxKind::MatchExpr
        );
        if !foldable {
            continue;
        }
        let r = node.text_range();
        let (start_byte, end_byte): (usize, usize) = (r.start().into(), r.end().into());
        let start_line = line_of(model, start_byte);
        let end_line = line_of(model, end_byte.saturating_sub(1));
        if end_line > start_line {
            out.push(FoldingRange {
                start_line,
                end_line,
                start_character: None,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }
    // 2. `//region` / `//endregion` comment-pair folds.
    out.extend(region_folds(model));
    out
}

/// 0-based line number containing byte `byte`.
fn line_of(model: &SemanticModel, byte: usize) -> u32 {
    // Char offset, then the line index gives a `Position` whose `.line` we want.
    let ch = crate::lsp::convert::byte_to_char(&model.text, byte);
    model.line_index.position(ch).line
}

/// Match `//region` lines with the next `//endregion` (LIFO nesting) from the
/// cached `LineComment` tokens.
fn region_folds(model: &SemanticModel) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    for el in model.tree.descendants_with_tokens() {
        let Some(tok) = el.into_token() else {
            continue;
        };
        if tok.kind() != SyntaxKind::LineComment {
            continue;
        }
        let text = tok.text().trim_start_matches('/').trim();
        let r = tok.text_range();
        let line = line_of(model, usize::from(r.start()));
        if text.starts_with("region") {
            stack.push(line);
        } else if text.starts_with("endregion") {
            if let Some(start_line) = stack.pop() {
                if line > start_line {
                    out.push(FoldingRange {
                        start_line,
                        end_line: line,
                        start_character: None,
                        end_character: None,
                        kind: Some(FoldingRangeKind::Region),
                        collapsed_text: None,
                    });
                }
            }
        }
    }
    out
}

use tower_lsp::lsp_types::SelectionRange;

/// The selection-range chain at byte `offset`: the innermost CST node containing
/// the offset, then each ancestor outward, as a linked `SelectionRange`. The LSP
/// client expands the selection up this chain.
pub fn selection_range_at(model: &SemanticModel, offset: usize) -> Option<SelectionRange> {
    // Innermost node whose range contains the offset.
    let innermost = model
        .tree
        .descendants()
        .filter(|n| {
            let r = n.text_range();
            let (s, e): (usize, usize) = (r.start().into(), r.end().into());
            offset >= s && offset < e
        })
        .min_by_key(|n| {
            let r = n.text_range();
            let (s, e): (usize, usize) = (r.start().into(), r.end().into());
            e - s
        })?;
    // Collect innermost + ancestors (cstree's `ancestors()` is self-inclusive, so
    // `dedup_by_key` collapses the duplicated innermost), then fold from the
    // OUTERMOST in so each `SelectionRange.parent` is the next-larger node.
    let mut nodes: Vec<_> = std::iter::once(innermost.clone())
        .chain(innermost.ancestors().cloned())
        .collect();
    nodes.dedup_by_key(|n| n.text_range());
    let mut chain: Option<SelectionRange> = None;
    for node in nodes.into_iter().rev() {
        let range = crate::lsp::convert::byte_span_to_range(
            &model.text,
            &model.line_index,
            crate::check::ByteSpan::from(node.text_range()),
        );
        chain = Some(SelectionRange {
            range,
            parent: chain.map(Box::new),
        });
    }
    chain
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn selection_expands_outward() {
        let src = "fn f() {\n  return 1 + 2\n}\n";
        let m = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let off = src.find("1 + 2").unwrap(); // on the `1`
        let sel = selection_range_at(&m, off).expect("selection");
        // The chain must be nested: each parent's range is wider than the child's.
        let inner = sel.range;
        let parent = sel.parent.as_ref().expect("has a parent").range;
        let inner_w = inner.end.character.saturating_sub(inner.start.character)
            + (inner.end.line - inner.start.line) * 1000;
        let parent_w = parent.end.character.saturating_sub(parent.start.character)
            + (parent.end.line - parent.start.line) * 1000;
        assert!(
            parent_w >= inner_w,
            "parent should be no smaller: {inner:?} {parent:?}"
        );
    }
}

#[cfg(test)]
mod folding_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn folds_a_multiline_fn_body() {
        let m = model("fn f() {\n  let x = 1\n  return x\n}\n");
        let folds = folding_ranges(&m);
        assert!(
            folds.iter().any(|f| f.start_line == 0 && f.end_line >= 2),
            "{folds:?}"
        );
    }

    #[test]
    fn folds_region_comments() {
        let src = "//region setup\nlet a = 1\nlet b = 2\n//endregion\n";
        let folds = folding_ranges(&model(src));
        assert!(
            folds.iter().any(|f| f.start_line == 0 && f.end_line == 3),
            "{folds:?}"
        );
    }

    #[test]
    fn single_line_block_not_folded() {
        let m = model("fn f() { return 1 }\n");
        let folds = folding_ranges(&m);
        // A one-line block has no fold (end_line == start_line).
        assert!(folds.is_empty(), "{folds:?}");
    }
}
