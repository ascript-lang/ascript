//! `textDocument/codeAction` (+ resolve) over `check::fix`. Exposes per-diagnostic
//! quickfixes (only codes in `FIXABLE_CODES` that carry a `Fix`), plus the
//! `source.organizeImports` and `source.fixAll` source actions. All edits are byte
//! spans translated to LSP ranges via `convert.rs`; the file's whole edit set is
//! applied with the same overlap-safe `apply_edits` the CLI `--fix` uses.

use crate::check::diagnostic::Fix;
use crate::lsp::model::SemanticModel;
use std::collections::HashMap;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, TextEdit, Url, WorkspaceEdit,
};

/// The command id that backs `source.fixAll` via `workspace/executeCommand`.
pub const FIX_ALL_COMMAND: &str = "ascript.fixAll";

/// One quickfix `CodeAction` per fixable diagnostic carrying a `Fix`.
pub fn quickfixes(model: &SemanticModel, uri: &Url) -> Vec<CodeActionOrCommand> {
    let mut out = Vec::new();
    for d in &model.diagnostics {
        if !crate::check::fix::FIXABLE_CODES.contains(&d.code.as_str()) {
            continue;
        }
        let Some(fix) = &d.fix else { continue };
        out.push(CodeActionOrCommand::CodeAction(quickfix_action(
            model, uri, fix,
        )));
    }
    out
}

/// Diagnostic codes whose carried `Fix` is a "did you mean" SUGGESTION (DX D4
/// §5.2) — offered as a quickfix in the editor but deliberately NOT in
/// `FIXABLE_CODES`, so `--fix`/`source.fixAll` never auto-applies a guess.
const SUGGESTION_CODES: &[&str] = &["undefined-variable"];

/// One quickfix `CodeAction` per "did you mean" suggestion diagnostic carrying a
/// `Fix` (DX D4 §5.2). Separate from [`quickfixes`] because these are guesses: the
/// user picks them in the editor, but they are excluded from the auto-apply
/// `fixAll`/`--fix` path.
pub fn suggestion_quickfixes(model: &SemanticModel, uri: &Url) -> Vec<CodeActionOrCommand> {
    let mut out = Vec::new();
    for d in &model.diagnostics {
        if !SUGGESTION_CODES.contains(&d.code.as_str()) {
            continue;
        }
        let Some(fix) = &d.fix else { continue };
        out.push(CodeActionOrCommand::CodeAction(quickfix_action(
            model, uri, fix,
        )));
    }
    out
}

/// Build a `quickfix` `CodeAction` from a `Fix` (its byte-span edits → LSP edits).
fn quickfix_action(model: &SemanticModel, uri: &Url, fix: &Fix) -> CodeAction {
    let edits: Vec<TextEdit> = fix
        .edits
        .iter()
        .map(|e| TextEdit {
            range: crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, e.range),
            new_text: e.replacement.clone(),
        })
        .collect();
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    CodeAction {
        title: fix.title.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }),
        ..CodeAction::default()
    }
}

use tower_lsp::lsp_types::{CodeActionContext, Position, Range};

/// A whole-document replacement edit set for `uri` → `new_text` (one `TextEdit`
/// from file start to file end).
fn whole_doc_edit(model: &SemanticModel, uri: &Url, new_text: String) -> WorkspaceEdit {
    let end = crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        crate::check::ByteSpan {
            start: model.text.len(),
            end: model.text.len(),
        },
    )
    .end;
    let edits = vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end,
        },
        new_text,
    }];
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    WorkspaceEdit {
        changes: Some(changes),
        ..WorkspaceEdit::default()
    }
}

/// `source.fixAll`: apply every fixable fix at once. `None` when nothing changes.
pub fn fix_all_action(model: &SemanticModel, uri: &Url) -> Option<CodeAction> {
    let analysis = crate::check::analyze::analyze(&model.text);
    let edits = crate::check::fix::collect_fixes(&analysis);
    if edits.is_empty() {
        return None;
    }
    let fixed = crate::check::fix::apply_edits(&model.text, &edits);
    if fixed == model.text {
        return None;
    }
    Some(CodeAction {
        title: "Fix all auto-fixable problems".to_string(),
        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
        edit: Some(whole_doc_edit(model, uri, fixed)),
        ..CodeAction::default()
    })
}

/// `source.organizeImports`: re-run the canonical formatter, which normalizes
/// import lines (canonical `import { a, b } from "…"` spacing/quotes). v1 reuses
/// the whole-file formatter; a dedicated import sorter is a later refinement.
pub fn organize_imports_action(model: &SemanticModel, uri: &Url) -> CodeAction {
    let formatted = crate::syntax::format::format(&model.tree);
    CodeAction {
        title: "Organize imports".to_string(),
        kind: Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS),
        edit: Some(whole_doc_edit(model, uri, formatted)),
        ..CodeAction::default()
    }
}

/// All code actions for `uri` over `range`, honoring `only` kinds in `ctx`.
pub fn code_actions(
    model: &SemanticModel,
    uri: &Url,
    _range: Range,
    ctx: &CodeActionContext,
) -> Vec<CodeActionOrCommand> {
    let wants = |k: &CodeActionKind| match &ctx.only {
        Some(only) => only.iter().any(|o| k.as_str().starts_with(o.as_str())),
        None => true,
    };
    let mut out = Vec::new();
    if wants(&CodeActionKind::QUICKFIX) {
        out.extend(quickfixes(model, uri));
        out.extend(suggestion_quickfixes(model, uri));
    }
    if wants(&CodeActionKind::SOURCE_FIX_ALL) {
        if let Some(a) = fix_all_action(model, uri) {
            out.push(CodeActionOrCommand::CodeAction(a));
        }
    }
    if wants(&CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
        out.push(CodeActionOrCommand::CodeAction(organize_imports_action(
            model, uri,
        )));
    }
    out
}

/// Fill a source action's `WorkspaceEdit` lazily. The action's `data` carries the
/// target `Url` (set by the cheap pass). `quickfix` actions already carry their
/// edit and pass through unchanged.
pub fn resolve_code_action(model: &SemanticModel, mut action: CodeAction) -> CodeAction {
    if action.edit.is_some() {
        return action;
    }
    let Some(data) = &action.data else {
        return action;
    };
    let Ok(uri) = serde_json::from_value::<Url>(data.clone()) else {
        return action;
    };
    action.edit = match action.kind.as_ref() {
        Some(k) if *k == CodeActionKind::SOURCE_FIX_ALL => {
            fix_all_action(model, &uri).and_then(|a| a.edit)
        }
        Some(k) if *k == CodeActionKind::SOURCE_ORGANIZE_IMPORTS => {
            organize_imports_action(model, &uri).edit
        }
        _ => None,
    };
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn offers_quickfix_for_unused_import() {
        // `b` is unused → an `unused-import` diagnostic with a removal fix.
        let m = model("import { a, b } from \"std/math\"\nprint(a(1))\n");
        let uri = Url::parse("file:///main.as").unwrap();
        let actions = quickfixes(&m, &uri);
        assert!(!actions.is_empty(), "expected an unused-import quickfix");
        let CodeActionOrCommand::CodeAction(ca) = &actions[0] else {
            panic!("expected a CodeAction");
        };
        assert_eq!(ca.kind, Some(CodeActionKind::QUICKFIX));
        assert!(ca.edit.is_some(), "quickfix carries a WorkspaceEdit");
    }

    #[test]
    fn offers_did_you_mean_quickfix() {
        // `lenght` is a typo of the local `length` → a suggestion quickfix.
        let m = model("let length = 5\nprint(lenght)\n");
        let uri = Url::parse("file:///main.as").unwrap();
        let actions = suggestion_quickfixes(&m, &uri);
        assert!(!actions.is_empty(), "expected a did-you-mean quickfix");
        let CodeActionOrCommand::CodeAction(ca) = &actions[0] else {
            panic!("expected a CodeAction");
        };
        assert_eq!(ca.kind, Some(CodeActionKind::QUICKFIX));
        assert!(ca.title.contains("length"), "title: {}", ca.title);
        let edit = ca.edit.as_ref().expect("quickfix carries an edit");
        let changes = edit.changes.as_ref().unwrap();
        assert_eq!(changes[&uri][0].new_text, "length");
    }

    #[test]
    fn did_you_mean_is_excluded_from_fix_all() {
        // A did-you-mean suggestion must NOT be auto-applied by `fixAll`/`--fix`.
        let m = model("print(lenght)\n");
        let uri = Url::parse("file:///main.as").unwrap();
        // `undefined-variable` is not in FIXABLE_CODES → no fixAll edit from it.
        // (`print` is the only near match; the point is fixAll ignores the code.)
        let none = fix_all_action(&m, &uri);
        assert!(
            none.is_none(),
            "fixAll must not auto-apply a did-you-mean guess"
        );
    }

    #[test]
    fn fix_all_replaces_whole_document_with_fixed_text() {
        let src = "import { a, b } from \"std/math\"\nprint(a(1))\n";
        let m = model(src);
        let uri = Url::parse("file:///main.as").unwrap();
        let ca = fix_all_action(&m, &uri).expect("a fixAll action when there are fixes");
        let edit = ca.edit.expect("workspace edit");
        let changes = edit.changes.expect("changes");
        let edits = &changes[&uri];
        assert_eq!(edits.len(), 1, "one full-document replacement");
        // The fixed text drops the unused `b`.
        assert!(
            !edits[0].new_text.contains(", b"),
            "unused import removed: {:?}",
            edits[0].new_text
        );
    }

    #[test]
    fn organize_imports_is_a_source_action() {
        let m = model("import { a } from \"std/math\"\nprint(a(1))\n");
        let uri = Url::parse("file:///main.as").unwrap();
        let ca = organize_imports_action(&m, &uri);
        assert_eq!(ca.kind, Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS));
    }

    #[test]
    fn resolve_fills_edit_for_fix_all() {
        let src = "import { a, b } from \"std/math\"\nprint(a(1))\n";
        let m = model(src);
        let uri = Url::parse("file:///main.as").unwrap();
        // A bare fixAll action with no edit (the cheap pass), tagged with the URI.
        let bare = CodeAction {
            title: "Fix all auto-fixable problems".to_string(),
            kind: Some(CodeActionKind::SOURCE_FIX_ALL),
            data: Some(serde_json::to_value(&uri).unwrap()),
            ..CodeAction::default()
        };
        let resolved = resolve_code_action(&m, bare);
        assert!(resolved.edit.is_some(), "resolve fills the edit");
    }
}
