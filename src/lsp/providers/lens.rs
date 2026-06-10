//! `textDocument/codeLens` (+ `codeLens/resolve`): a "▶ Run test" lens above each
//! `test("name", …)` registration, a "▶ Run all tests" lens for a file that has any
//! `test(...)`, a "▶ Run" lens above `main`, and a reference-count lens above each
//! top-level declaration. The run lenses carry the `ascript.runTest` / `ascript.run`
//! commands (backed by `executeCommand`); the count lens is resolved lazily via the
//! workspace ref count.
//!
//! Command/argument convention (matched to the server's `execute_command` handler and
//! the VS Code extension's `runInTerminal`):
//! - `ascript.run` `[uri]` → `ascript run <file>`;
//! - `ascript.runTest` `[uri, name]` → `ascript test <file> --filter <name>` (one test);
//! - `ascript.runTest` `[uri]` → `ascript test <file>` (all tests in the file).

use crate::check::ByteSpan;
use crate::lsp::model::SemanticModel;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::kind::SyntaxKind;
use serde_json::json;
use tower_lsp::lsp_types::{CodeLens, Command, Range};

/// The (unresolved) lenses for `model`. The run lenses are fully resolved here;
/// the reference-count lenses carry `data` and are completed in `codeLens/resolve`.
pub fn code_lenses(model: &SemanticModel, uri: &str) -> Vec<CodeLens> {
    let mut out = Vec::new();
    // 1. Run-test lenses: top-level `test("name", fn)` calls. The FIRST such call also
    //    anchors a single "▶ Run all tests" lens for the whole file.
    let mut first_test_call: Option<ByteSpan> = None;
    for call in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::CallExpr)
    {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        if crate::syntax::resolve::ident_text(callee).as_deref() != Some("test") {
            continue;
        }
        let Some(args) = call.children().find(|c| c.kind() == SyntaxKind::ArgList) else {
            continue;
        };
        let Some(name_lit) = args.children().find(|c| c.kind() == SyntaxKind::Literal) else {
            continue;
        };
        let Some(test_name) = string_literal_body(name_lit) else {
            continue;
        };
        let call_span = ByteSpan::from(call.text_range());
        if first_test_call.is_none() {
            first_test_call = Some(call_span);
        }
        out.push(CodeLens {
            range: line_start_range(model, call_span),
            command: Some(Command {
                title: "▶ Run test".to_string(),
                command: "ascript.runTest".to_string(),
                // [uri, test_name]: the editor runs `ascript test <file> --filter <name>`.
                arguments: Some(vec![json!(uri), json!(test_name)]),
            }),
            data: None,
        });
    }
    // 1b. "▶ Run all tests": one lens for the whole file when it has any `test(...)`
    //     registration. It reuses the already server-handled `ascript.runTest` command
    //     id but with ONLY the URI (no test name → no `--filter` → `ascript test <file>`
    //     runs them all). Anchored to the first test call's line.
    if let Some(span) = first_test_call {
        out.push(CodeLens {
            range: line_start_range(model, span),
            command: Some(Command {
                title: "▶ Run all tests".to_string(),
                command: "ascript.runTest".to_string(),
                arguments: Some(vec![json!(uri)]),
            }),
            data: None,
        });
    }
    // 2. Run lens above a top-level `fn main`.
    for decl in model.tree.children().filter(|n| n.kind() == SyntaxKind::FnDecl) {
        if crate::syntax::resolve::ident_text(decl).as_deref() == Some("main") {
            out.push(CodeLens {
                range: line_start_range(model, ByteSpan::from(decl.text_range())),
                command: Some(Command {
                    title: "▶ Run".to_string(),
                    command: "ascript.run".to_string(),
                    arguments: Some(vec![json!(uri)]),
                }),
                data: None,
            });
        }
    }
    // 3. Reference-count lens above each top-level decl (resolved lazily).
    for decl in model.tree.children() {
        if !matches!(
            decl.kind(),
            SyntaxKind::FnDecl | SyntaxKind::ClassDecl | SyntaxKind::EnumDecl
        ) {
            continue;
        }
        let Some(name) = crate::syntax::resolve::ident_text(decl) else {
            continue;
        };
        out.push(CodeLens {
            range: line_start_range(model, ByteSpan::from(decl.text_range())),
            command: None, // unresolved
            data: Some(json!({ "kind": "refs", "uri": uri, "name": name })),
        });
    }
    out
}

/// Resolve a reference-count lens by counting same-file `NameRef` uses of its name.
/// (Cross-file counts are added by the server, which has the workspace index.)
pub fn resolve_same_file_ref_count(model: &SemanticModel, name: &str) -> usize {
    model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NameRef)
        .filter(|n| crate::syntax::resolve::ident_text(n).as_deref() == Some(name))
        .count()
}

fn string_literal_body(lit: &ResolvedNode) -> Option<String> {
    let tok = lit
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Str)?;
    let raw = tok.text();
    let q = raw.chars().next()?;
    if (q != '"' && q != '\'') || raw.len() < 2 || !raw.ends_with(q) {
        return None;
    }
    Some(raw[1..raw.len() - 1].to_string())
}

/// A zero-width range at the START of the line `span` begins on (lenses render
/// above the line).
fn line_start_range(model: &SemanticModel, span: ByteSpan) -> Range {
    let r = crate::lsp::convert::byte_span_to_range(&model.text, &model.line_index, span);
    Range {
        start: tower_lsp::lsp_types::Position { line: r.start.line, character: 0 },
        end: r.start,
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
    fn run_test_lens_for_test_call() {
        let m = model("test(\"adds\", fn() { return 1 })\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let run = lenses
            .iter()
            .find(|l| l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.runTest"));
        let cmd = run.expect("run-test lens").command.as_ref().unwrap();
        assert_eq!(cmd.arguments.as_ref().unwrap()[1], json!("adds"));
    }

    #[test]
    fn run_lens_for_main() {
        let m = model("fn main() {}\n");
        let lenses = code_lenses(&m, "file:///t.as");
        assert!(lenses
            .iter()
            .any(|l| l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.run")));
    }

    #[test]
    fn run_test_lens_pins_title_command_and_arguments() {
        // DX D3 Task 14: the per-test "▶ Run test" lens carries the file URI + the
        // test NAME (so the editor can pass `--filter <name>` to `ascript test`).
        let m = model("test(\"adds\", fn() { return 1 })\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let run = lenses
            .iter()
            .find(|l| l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.runTest"))
            .expect("run-test lens");
        let cmd = run.command.as_ref().unwrap();
        assert_eq!(cmd.title, "▶ Run test");
        assert_eq!(cmd.command, "ascript.runTest");
        assert_eq!(
            cmd.arguments.as_ref().unwrap(),
            &vec![json!("file:///t.as"), json!("adds")],
            "per-test lens args = [uri, test_name]"
        );
    }

    #[test]
    fn run_lens_for_main_pins_title_command_and_arguments() {
        let m = model("fn main() {}\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let run = lenses
            .iter()
            .find(|l| l.command.as_ref().map(|c| c.command.as_str()) == Some("ascript.run"))
            .expect("run lens over main");
        let cmd = run.command.as_ref().unwrap();
        assert_eq!(cmd.title, "▶ Run");
        assert_eq!(cmd.command, "ascript.run");
        assert_eq!(cmd.arguments.as_ref().unwrap(), &vec![json!("file:///t.as")]);
    }

    #[test]
    fn run_all_tests_lens_present_for_a_file_with_tests() {
        // DX D3 Task 14: a file with `test(...)` registrations gets a single
        // "▶ Run all tests" lens whose command runs the whole file's tests. It reuses
        // the already-handled `ascript.runTest` command id with ONLY the URI (no test
        // name → no `--filter` → all tests run).
        let m = model("test(\"a\", fn() {})\ntest(\"b\", fn() {})\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let all: Vec<&CodeLens> = lenses
            .iter()
            .filter(|l| {
                l.command.as_ref().map(|c| c.title.as_str()) == Some("▶ Run all tests")
            })
            .collect();
        assert_eq!(all.len(), 1, "exactly one Run-all-tests lens");
        let cmd = all[0].command.as_ref().unwrap();
        assert_eq!(cmd.command, "ascript.runTest");
        assert_eq!(
            cmd.arguments.as_ref().unwrap(),
            &vec![json!("file:///t.as")],
            "run-all lens args = [uri] only (no test name)"
        );
    }

    #[test]
    fn no_run_all_tests_lens_when_no_tests() {
        let m = model("fn main() {}\n");
        let lenses = code_lenses(&m, "file:///t.as");
        assert!(
            lenses
                .iter()
                .all(|l| l.command.as_ref().map(|c| c.title.as_str()) != Some("▶ Run all tests")),
            "no Run-all-tests lens without any test() registrations"
        );
    }

    #[test]
    fn ref_count_lens_is_unresolved() {
        let m = model("fn helper() {}\nhelper()\nhelper()\n");
        let lenses = code_lenses(&m, "file:///t.as");
        let refs = lenses.iter().find(|l| l.data.is_some()).expect("a refs lens");
        assert!(refs.command.is_none(), "ref lens starts unresolved");
        // helper appears as 2 call uses → count the NameRef uses.
        assert!(resolve_same_file_ref_count(&m, "helper") >= 2);
    }
}
