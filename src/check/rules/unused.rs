//! `unused-binding` / `unused-import`: a binding with zero read uses. Parameters
//! are exempt (often intentionally unused). Imports/lets get a removal fix.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Fix, Severity, TextEdit};
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use crate::syntax::resolve::types::{Binding, BindingKind, ResolveResult};
use cstree::text::TextRange;

pub fn check(tree: &ResolvedNode, resolved: &ResolveResult, src: &str) -> Vec<AsDiagnostic> {
    let mut out = Vec::new();
    for b in &resolved.bindings {
        if b.use_count != 0 {
            continue;
        }
        // An underscore-prefixed name (incl. bare `_`) is the conventional
        // "intentionally unused" marker — never flag it.
        if b.name.starts_with('_') {
            continue;
        }
        match b.kind {
            BindingKind::Param => {} // params are exempt
            BindingKind::Import => out.push(unused_import(b, tree, src)),
            BindingKind::Let | BindingKind::Const => {
                // `unused-binding` keeps its name-span fix (LSP-only — NOT in
                // `FIXABLE_CODES`, so `--fix` never applies it: removing
                // `let x = sideEffect()` would drop the side effect).
                out.push(unused_name_span(b, "unused-binding", "remove unused binding"))
            }
            // PatternBind is exempt: destructuring a `[value, err]` Result pair (or
            // an object/array) idiomatically binds slots that aren't all read.
            // fn/class/enum/loop-var: skip (often public API / loop counters).
            _ => {}
        }
    }
    out
}

/// A diagnostic whose fix replaces the binding's NAME span (LSP code-action only).
fn unused_name_span(b: &Binding, code: &str, fix_title: &str) -> AsDiagnostic {
    let range = ByteSpan::from(b.decl_range);
    AsDiagnostic {
        range,
        severity: Severity::Warning,
        code: code.to_string(),
        message: format!("`{}` is never used", b.name),
        fix: Some(Fix {
            title: fix_title.to_string(),
            edits: vec![TextEdit {
                range,
                replacement: String::new(),
            }],
        }),
    }
}

/// The `unused-import` diagnostic. Its fix removes a REMOVABLE unit (the whole
/// `import` statement, or one clause of a multi-name `import { a, b }` list) —
/// never just the name token, which would leave a syntax error (`import {  }`).
/// The diagnostic's own RANGE stays the name span (for the caret); only the FIX's
/// edit covers the larger removal unit.
fn unused_import(b: &Binding, tree: &ResolvedNode, src: &str) -> AsDiagnostic {
    // The resolver records EVERY import binding with `decl_range` = the whole
    // `ImportStmt` range (not the name token), so the diagnostic caret and the
    // removal range are both computed from the statement + the binding NAME.
    let (caret, edit_range) = import_ranges(&b.name, b.decl_range, tree, src);
    AsDiagnostic {
        range: caret,
        severity: Severity::Warning,
        code: "unused-import".to_string(),
        message: format!("`{}` is never used", b.name),
        fix: Some(Fix {
            title: "remove unused import".to_string(),
            edits: vec![TextEdit {
                range: edit_range,
                replacement: String::new(),
            }],
        }),
    }
}

/// Compute `(caret, edit)` byte ranges for an unused import named `name` whose
/// binding `decl_range` is the enclosing `ImportStmt`:
///
/// - `caret` — the imported NAME token (so the diagnostic points at the name,
///   not the whole statement). Falls back to the statement range if the token
///   can't be found.
/// - `edit` — the range to DELETE:
///   - a **multi-name** clause (`import { a, b }`, removing `a`) → the name token
///     plus its adjacent comma (following `, ` preferred, else preceding ` ,`),
///     keeping the list well-formed (`import { b } ...`);
///   - a **single-name** list or a **namespace** `import * as t` → the whole
///     `ImportStmt`, extended over its trailing newline (no blank line left).
fn import_ranges(
    name: &str,
    decl_range: TextRange,
    tree: &ResolvedNode,
    src: &str,
) -> (ByteSpan, ByteSpan) {
    use SyntaxKind::*;
    let stmt_span = ByteSpan::from(decl_range);
    // Find the `ImportStmt` whose range matches the binding's decl_range.
    let Some(stmt) = tree
        .descendants()
        .find(|n| n.kind() == ImportStmt && n.text_range() == decl_range)
    else {
        return (stmt_span, stmt_span);
    };

    // Multi-name list → caret = the matching name token; edit = name + a comma.
    if let Some(list) = stmt.children().find(|c| c.kind() == ImportList) {
        let name_idents: Vec<_> = list
            .children_with_tokens()
            .filter_map(|el| el.into_token().cloned())
            .filter(|t| t.kind() == Ident)
            .collect();
        let name_tok = name_idents.iter().find(|t| t.text() == name).cloned();
        if name_idents.len() > 1 {
            if let Some(tok) = &name_tok {
                let caret = ByteSpan::from(tok.text_range());
                return (caret, clause_removal_range(list, tok.text_range()));
            }
        }
        // Single-name list → caret = the name token if found, edit = whole stmt.
        let caret = name_tok
            .map(|t| ByteSpan::from(t.text_range()))
            .unwrap_or(stmt_span);
        return (caret, whole_stmt_removal(stmt, src));
    }

    // Namespace import `import * as t`: caret = the alias token if found.
    let caret = stmt
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .filter(|t| t.kind() == Ident)
        .find(|t| t.text() == name)
        .map(|t| ByteSpan::from(t.text_range()))
        .unwrap_or(stmt_span);
    (caret, whole_stmt_removal(stmt, src))
}

/// The whole-`ImportStmt` removal range, extended to swallow the trailing newline
/// (and any same-line trailing CR) so removing the import leaves no blank line.
fn whole_stmt_removal(stmt: &ResolvedNode, src: &str) -> ByteSpan {
    let full = ByteSpan::from(stmt.text_range());
    let bytes = src.as_bytes();
    let mut end = full.end;
    if end < bytes.len() && bytes[end] == b'\r' {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    }
    ByteSpan {
        start: full.start,
        end,
    }
}

/// The removal range for ONE clause of a multi-name `import { a, b }` list: the
/// name token at `name_range` plus its adjacent comma. Prefer the FOLLOWING
/// `, name` (so `{ a, b }` removing `a` → `{ b }`); if the name is last, take the
/// PRECEDING `name ,` (so removing `b` → `{ a }`). Walks the `ImportList`'s
/// tokens to find the name's position and the nearest comma on either side.
fn clause_removal_range(list: &ResolvedNode, name_range: TextRange) -> ByteSpan {
    use SyntaxKind::*;
    // Collect the list's direct tokens in source order (Ident/Comma/whitespace/braces).
    let toks: Vec<_> = list
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .collect();
    // Index of the target name token.
    let Some(name_idx) = toks
        .iter()
        .position(|t| t.kind() == Ident && t.text_range() == name_range)
    else {
        return ByteSpan::from(name_range);
    };

    let name_start = usize::from(name_range.start());
    let name_end = usize::from(name_range.end());

    // Look RIGHT for the next comma (skipping whitespace); its end is the range end.
    let following_comma = toks
        .iter()
        .skip(name_idx + 1)
        .find(|t| t.kind() == Comma)
        // stop the search at the next Ident — a comma must come before any next name
        .filter(|_| {
            toks.iter()
                .skip(name_idx + 1)
                .take_while(|t| t.kind() != Ident)
                .any(|t| t.kind() == Comma)
        });
    if let Some(comma) = following_comma {
        // Extend past the comma AND any whitespace up to the NEXT name, so
        // `{ a, b }` removing `a` yields `{ b }` (no leftover double space).
        let after_comma = usize::from(comma.text_range().end());
        let next_name_start = toks
            .iter()
            .skip(name_idx + 1)
            .find(|t| t.kind() == Ident)
            .map(|t| usize::from(t.text_range().start()))
            .unwrap_or(after_comma);
        return ByteSpan {
            start: name_start,
            end: next_name_start.max(after_comma),
        };
    }

    // No following comma (the name is the LAST clause): take the PRECEDING comma
    // (and any whitespace before this name) up to the name's end, so `{ a, b }`
    // removing `b` yields `{ a }`.
    let preceding_comma = toks
        .iter()
        .take(name_idx)
        .rev()
        .find(|t| t.kind() == Comma);
    if let Some(comma) = preceding_comma {
        return ByteSpan {
            start: usize::from(comma.text_range().start()),
            end: name_end,
        };
    }

    // Defensive fallback (shouldn't happen for a >1-name list): just the name.
    ByteSpan {
        start: name_start,
        end: name_end,
    }
}

#[cfg(test)]
mod tests {
    use crate::check::analyze;
    fn codes(src: &str) -> Vec<String> {
        analyze(src)
            .diagnostics
            .into_iter()
            .map(|d| d.code)
            .collect()
    }
    #[test]
    fn flags_unused_let() {
        assert!(codes("let x = 1\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn used_let_not_flagged() {
        assert!(!codes("let x = 1\nprint(x)\n").contains(&"unused-binding".to_string()));
    }
    #[test]
    fn flags_unused_import() {
        assert!(codes("import * as t from \"std/task\"\nprint(1)\n")
            .contains(&"unused-import".to_string()));
    }
    #[test]
    fn unused_param_is_exempt() {
        assert!(!codes("fn f(a) { return 1 }\nf(0)\n").contains(&"unused-binding".to_string()));
    }

    // ---- A2: the import fix must remove a removable unit, never just the name ---

    use crate::check::{apply_edits, collect_fixes};

    /// Apply every fixable autofix to `src` and return the rewritten source.
    fn fixed(src: &str) -> String {
        apply_edits(src, &collect_fixes(&analyze(src)))
    }

    #[test]
    fn multi_name_import_fix_removes_only_the_unused_clause() {
        // `a` unused, `b` used → fix yields `import { b } from "std/math"`,
        // which re-analyzes with NO syntax-error and NO unused-import.
        let src = "import { a, b } from \"std/math\"\nprint(b)\n";
        let after = fixed(src);
        // The unused name `a` is gone from the import clause; `b` remains.
        assert_eq!(after, "import { b } from \"std/math\"\nprint(b)\n", "after: {after:?}");
        let re = analyze(&after);
        assert!(
            !re.diagnostics
                .iter()
                .any(|d| d.code == "syntax-error" || d.code == "unused-import"),
            "re-analyze: {:?}",
            re.diagnostics
        );
    }

    #[test]
    fn multi_name_import_fix_removes_leading_unused_keeps_trailing() {
        // `b` unused (trailing), `a` used → fix yields `import { a } from ...`.
        let src = "import { a, b } from \"std/math\"\nprint(a)\n";
        let after = fixed(src);
        let re = analyze(&after);
        assert!(
            !re.diagnostics
                .iter()
                .any(|d| d.code == "syntax-error" || d.code == "unused-import"),
            "after: {after:?}, re-analyze: {:?}",
            re.diagnostics
        );
        assert!(after.contains('a'), "after: {after:?}");
    }

    #[test]
    fn namespace_import_fix_removes_whole_statement_no_blank_line() {
        // `import * as t` unused → fix removes the whole line (incl. newline), so
        // `print(1)` remains with no leading blank line.
        let src = "import * as t from \"std/task\"\nprint(1)\n";
        let after = fixed(src);
        assert_eq!(after, "print(1)\n", "after: {after:?}");
    }

    #[test]
    fn single_name_import_fix_removes_whole_statement() {
        // `import { a }` (a unused, the only name) → remove the whole statement.
        let src = "import { a } from \"std/math\"\nprint(1)\n";
        let after = fixed(src);
        assert_eq!(after, "print(1)\n", "after: {after:?}");
        assert!(analyze(&after).diagnostics.is_empty(), "{after:?}");
    }
}
