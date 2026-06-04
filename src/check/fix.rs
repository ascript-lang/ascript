//! Autofix application for `ascript check --fix`.
//!
//! Feature-independent (the checker core builds under `--no-default-features`):
//! this module is serde-free and front-end/interpreter-free, operating purely on
//! `&str` source + the neutral `TextEdit` model.
//!
//! Only diagnostics whose `code` is in [`FIXABLE_CODES`] AND that carry a `fix`
//! are applied — the allowlist (not merely "has a fix") gates application, so a
//! rule whose textual fix is unsafe to auto-apply (e.g. `unused-binding`, which
//! could drop a side-effecting initializer) can still emit a `Fix` for LSP
//! code-actions without `--fix` ever touching it.

use crate::check::diagnostic::TextEdit;
use crate::check::Analysis;

/// Diagnostic codes whose emitted fix is safe to auto-apply with `--fix`.
///
/// v1: `unused-import` only. Removing an unused import is non-destructive (an
/// import is a pure binding with no runtime side effect). `unused-binding`
/// removal is deliberately EXCLUDED — `let x = sideEffect()` would drop the call.
pub const FIXABLE_CODES: &[&str] = &["unused-import"];

/// Gather the edits of every diagnostic whose code is in [`FIXABLE_CODES`] and
/// that carries a `fix`. The edits are returned in diagnostic order; the
/// applicator ([`apply_edits`]) handles ordering and overlap.
pub fn collect_fixes(analysis: &Analysis) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    for d in &analysis.diagnostics {
        if !FIXABLE_CODES.contains(&d.code.as_str()) {
            continue;
        }
        if let Some(fix) = &d.fix {
            edits.extend(fix.edits.iter().cloned());
        }
    }
    edits
}

/// Apply `edits` to `src`, returning the rewritten source.
///
/// Overlap-safe by construction:
/// 1. Edits are applied right-to-left (sorted by `range.start` descending) so
///    splicing one edit never invalidates the byte offsets of an earlier one.
/// 2. Any edit that overlaps a previously-applied edit (intersection on the
///    half-open `[start, end)`) is DROPPED. Adjacent (touching, non-overlapping)
///    edits both apply.
/// 3. Each surviving `replacement` is spliced in by byte range (`ByteSpan` is
///    byte offsets throughout the checker).
///
/// An empty edit list returns `src` unchanged.
pub fn apply_edits(src: &str, edits: &[TextEdit]) -> String {
    if edits.is_empty() {
        return src.to_string();
    }
    // Sort a copy by start offset descending; ties broken by end descending so a
    // wider edit at the same start is considered first (and a nested narrower one
    // is then dropped as overlapping).
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by(|a, b| {
        b.range
            .start
            .cmp(&a.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });

    let mut out = src.to_string();
    // The lowest byte offset touched by any already-applied edit. An edit is
    // dropped if its end would intrude past this point (overlap). `usize::MAX`
    // means nothing applied yet.
    let mut lowest_applied = usize::MAX;
    for e in sorted {
        let (start, end) = (e.range.start, e.range.end);
        // Defensive: ignore a malformed (inverted / out-of-bounds) range rather
        // than panic on a bad splice.
        if start > end || end > out.len() {
            continue;
        }
        // Overlap with a previously-applied (further-right) edit: its end must
        // not exceed the lowest start we've already consumed.
        if end > lowest_applied {
            continue;
        }
        // Byte-boundary safety: only splice on valid char boundaries.
        if !out.is_char_boundary(start) || !out.is_char_boundary(end) {
            continue;
        }
        out.replace_range(start..end, &e.replacement);
        lowest_applied = start;
    }
    out
}

/// A minimal, dependency-free unified line diff for `--fix-dry-run`.
///
/// Matches the hand-rolled JSON renderer's serde-free posture. The output is a
/// `--- a/<path>` / `+++ b/<path>` header followed by `-`/`+`/` ` prefixed lines
/// over the line-level LCS of `before` and `after`. Identical files produce only
/// the header (no body).
pub fn render_diff(path: &str, before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    if a == b {
        return out;
    }
    // Line-level LCS table (small inputs; the source files are human-scale).
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push_str(&format!(" {}\n", a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push_str(&format!("-{}\n", a[i]));
        i += 1;
    }
    while j < m {
        out.push_str(&format!("+{}\n", b[j]));
        j += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::diagnostic::{ByteSpan, TextEdit};

    fn edit(start: usize, end: usize, replacement: &str) -> TextEdit {
        TextEdit {
            range: ByteSpan { start, end },
            replacement: replacement.to_string(),
        }
    }

    #[test]
    fn single_edit_splices_by_byte_range() {
        // Replace "world" with "there" in "hello world".
        let out = apply_edits("hello world", &[edit(6, 11, "there")]);
        assert_eq!(out, "hello there");
    }

    #[test]
    fn two_non_overlapping_edits_apply_regardless_of_order() {
        let src = "abcdef";
        // Replace [0,2) with "X" and [4,6) with "Y" → "Xcd Y" style.
        let e1 = edit(0, 2, "X");
        let e2 = edit(4, 6, "Y");
        let forward = apply_edits(src, &[e1.clone(), e2.clone()]);
        let reverse = apply_edits(src, &[e2, e1]);
        assert_eq!(forward, "XcdY");
        assert_eq!(reverse, "XcdY");
    }

    #[test]
    fn nested_overlapping_edit_is_dropped() {
        let src = "abcdefgh";
        // Two overlapping edits: outer [2,6) and nested inner [3,5). Exactly one
        // survives (the applicator drops the overlapping one); the output is
        // well-formed and equals applying ONLY the surviving edit. Per the
        // descending-by-start policy the inner (larger start) sorts first and
        // survives, so the result equals applying just the inner edit.
        let outer = edit(2, 6, "XY");
        let inner = edit(3, 5, "Z");
        let out = apply_edits(src, &[outer.clone(), inner.clone()]);
        assert_eq!(out, apply_edits(src, std::slice::from_ref(&inner)), "out: {out}");
        assert_eq!(out, "abcZfgh");
        // Order-independent: the same two edits in the other input order produce
        // the same well-formed result (one survivor, never a double splice).
        assert_eq!(apply_edits(src, &[inner, outer]), out);
    }

    #[test]
    fn adjacent_touching_edits_both_apply() {
        let src = "abcdef";
        // [0,3) and [3,6) touch but do not overlap → both apply.
        let out = apply_edits(src, &[edit(0, 3, "X"), edit(3, 6, "Y")]);
        assert_eq!(out, "XY");
    }

    #[test]
    fn empty_edit_list_returns_input_unchanged() {
        assert_eq!(apply_edits("unchanged", &[]), "unchanged");
    }

    #[test]
    fn render_diff_identical_is_header_only() {
        let d = render_diff("f.as", "a\nb\n", "a\nb\n");
        assert_eq!(d, "--- a/f.as\n+++ b/f.as\n");
    }

    #[test]
    fn render_diff_shows_removed_line() {
        let d = render_diff("f.as", "keep\ndrop\nkeep2\n", "keep\nkeep2\n");
        assert!(d.contains("-drop"), "diff: {d}");
        assert!(d.contains(" keep"), "diff: {d}");
        assert!(d.contains(" keep2"), "diff: {d}");
    }
}
