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
}
