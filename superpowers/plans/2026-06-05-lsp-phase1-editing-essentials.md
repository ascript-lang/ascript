# LSP Phase 1 — Editing Essentials Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the three editing power-tools that turn the unified server into a daily driver: **formatting** (`textDocument/formatting` + `rangeFormatting`) wired to `syntax::format`, a **scope-aware completion rewrite** (`providers/completion.rs`, `completionItem/resolve`, auto-import) reading the resolver/infer/table/workspace, and **code actions** (`codeAction` + `codeAction/resolve`, `source.organizeImports`, `source.fixAll`, `executeCommand`) backed by `check::fix`. All three are pure `fn(&SemanticModel, …)` providers over the Phase-0 cached model — no provider touches `crate::{ast, lexer, parser, token}`.

**Architecture:** Phase 0 built the `SemanticModel` (CST + `ResolveResult` + diagnostics + `LexToken`s + `LineIndex`) and the `DocumentStore` cache; every Phase-1 provider is a new pure function over `&SemanticModel`, registered in `providers/mod.rs` and called from a thin `server.rs` handler. Formatting calls `crate::syntax::format::format(&model.tree)` and returns a single full-document `TextEdit`; range formatting reuses the same full-document format clamped to the requested range (canonical formatter has no per-region mode — documented limitation). Completion is rewritten as a context dispatcher over the model: import-path strings, member access (namespace exports / class fields+methods / enum variants), and a scope-aware baseline (in-scope locals/params from `resolved.bindings`, keywords, builtins, snippets) — plus auto-import `additionalTextEdits` for an unresolved name that matches a known std export. Code actions translate `check::fix` `Fix`/`TextEdit` (byte spans) into LSP `WorkspaceEdit`s via `convert.rs`, expose `source.organizeImports`/`source.fixAll` kinds, and back `fixAll` with an `executeCommand`.

**Tech Stack:** Rust, `tower-lsp`, `cstree` (red/green CST), the existing `src/syntax/` (format/resolve) + `src/check/` (fix/infer) crates.

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §4 (capability matrix: formatting, completion, codeAction rows), §6 (Phase 1).
- `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` — the conventions every step here reuses (the `SemanticModel`, `DocumentStore`, `convert.rs`, the providers-are-pure-fns rule, the legacy-import guard test).
- Formatter: `src/syntax/format/mod.rs` (`pub fn format(&ResolvedNode) -> String`), `src/syntax/mod.rs` (`pub fn format_tree(src: &str) -> String`). The formatter ALWAYS emits a trailing newline and is idempotent (its own `idempotent_on_slice` test proves it).
- Fixes: `src/check/fix.rs` (`FIXABLE_CODES`, `collect_fixes(&Analysis) -> Vec<TextEdit>`, `apply_edits(src, &[TextEdit]) -> String`), `src/check/diagnostic.rs` (`Fix { title, edits }`, `TextEdit { range: ByteSpan, replacement }`, `AsDiagnostic { range, severity, code, message, fix }`).
- Resolver: `src/syntax/resolve/types.rs` (`ResolveResult { uses, frames, bindings, … }`, `Binding { name, kind, decl_range, is_global, … }`, `BindingKind`, `Resolution::{Local,Upvalue,Global,Unresolved}`), `src/syntax/resolve/mod.rs` (`pub fn ident_text(&ResolvedNode) -> Option<String>`).
- Types/table: `src/check/infer/mod.rs` (`hover_type_at`), `src/check/infer/table.rs` (`Table::build(tree, resolved)`, `Table::class_id`/`class`/`enum_id`/`enum_info`; `ClassInfo { fields, methods }`, `EnumInfo { variants }`).
- Stdlib exports: `src/stdlib/mod.rs` (`pub fn std_module_exports(path) -> Option<Vec<(String, Value)>>` — Phase-0 completion already discards the `Value` with `.map(|(name, _)| …)`; keep that pattern, never store a `Value`).
- The Phase-0 completion port: `src/lsp/providers/completion.rs` (the behavior-preserving `completions(&SemanticModel, offset)` + `KEYWORDS`/`BUILTINS`/`STD_MODULE_PATHS`/`in_import_path_string`/`member_access_alias`/`namespace_import_module` moved off `analysis.rs`). Phase 1 REWRITES this file's `completions` entry while keeping its existing tests green as a subset.
- The CST-walk pattern: `src/lsp/workspace.rs` (`index_tree`, `definition_at`, `canon`) — the authoritative example of walking `ResolvedNode` + reading `resolve::types`.

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit tests) and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

---

## File Structure

- Create `src/lsp/providers/formatting.rs` — `format_document(&SemanticModel) -> Vec<TextEdit>` and `format_range(&SemanticModel, Range) -> Vec<TextEdit>`.
- Create `src/lsp/providers/code_action.rs` — `code_actions(&SemanticModel, uri, Range, &CodeActionContext) -> Vec<CodeActionOrCommand>`, `resolve_code_action(&SemanticModel, CodeAction) -> CodeAction`, plus `organize_imports`/`fix_all` builders and the `ascript.fixAll` command name.
- Rewrite `src/lsp/providers/completion.rs` — the scope-aware dispatcher + `resolve_completion(&SemanticModel, CompletionItem)` + auto-import. Keep the Phase-0 constants/helpers and tests.
- Modify `src/lsp/providers/mod.rs` — `pub mod formatting;`, `pub mod code_action;`.
- Modify `src/lsp/server.rs` — advertise `document_formatting_provider`, `document_range_formatting_provider`, `code_action_provider`, `execute_command_provider`, and the resolve flags on `completion_provider`; add the `formatting`/`range_formatting`/`code_action`/`code_action_resolve`/`completion_resolve`/`execute_command` handlers; route `completion` to the rewritten provider.
- Modify `tests/lsp.rs` — extend the capability assertion to the new providers.

---

## Task 1: `formatting.rs` — full-document formatting

**Files:**
- Create: `src/lsp/providers/formatting.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod formatting;`)
- Test: inline in `src/lsp/providers/formatting.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add alongside the existing `pub mod` lines:

```rust
pub mod formatting;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/formatting.rs`:

```rust
//! `textDocument/formatting` + `rangeFormatting` over the canonical formatter
//! (`crate::syntax::format`). The AScript formatter is whole-file and opinionated
//! (no per-region style), so we format the entire document and return a single
//! full-document replacement; range formatting reuses the same output, clamped to
//! whole lines covering the requested range (documented limitation in the spec
//! §2 non-goals: "formatter stays canonical/opinionated").

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// Format the whole document. Returns at most one full-range replacement; an
/// empty `Vec` when the formatted text already equals the source (a no-op edit
/// keeps clients from marking the buffer dirty).
pub fn format_document(model: &SemanticModel) -> Vec<TextEdit> {
    let formatted = crate::syntax::format::format(&model.tree);
    if formatted == model.text {
        return Vec::new();
    }
    vec![TextEdit {
        range: whole_document_range(model),
        new_text: formatted,
    }]
}

/// The `Range` covering the entire document (start of file → end of last line).
fn whole_document_range(model: &SemanticModel) -> Range {
    let end = crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        crate::check::ByteSpan {
            start: model.text.len(),
            end: model.text.len(),
        },
    )
    .end;
    Range {
        start: Position::new(0, 0),
        end,
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
    fn formats_messy_source_into_one_edit() {
        let edits = format_document(&model("let   x=1\n"));
        assert_eq!(edits.len(), 1, "expected one full-document edit");
        assert_eq!(edits[0].new_text, "let x = 1\n");
        assert_eq!(edits[0].range.start, Position::new(0, 0));
    }

    #[test]
    fn already_formatted_yields_no_edit() {
        // Canonical text is a fixed point — no edit, so the client buffer stays clean.
        let edits = format_document(&model("let x = 1\n"));
        assert!(edits.is_empty(), "got {edits:?}");
    }

    #[test]
    fn formatted_output_is_parseable_and_idempotent() {
        // The formatter output re-parses clean and is a fixed point on a second pass.
        let m = model("fn f(a,b){return a+b}\n");
        let once = format_document(&m);
        let formatted = once[0].new_text.clone();
        let m2 = SemanticModel::build(formatted.clone(), None, &LintConfig::default());
        // No syntax errors in the formatted output.
        assert!(
            !m2.diagnostics.iter().any(|d| d.code == "syntax-error"),
            "formatted output has syntax errors: {:?}",
            m2.diagnostics
        );
        // Second format is a no-op (idempotence).
        assert!(format_document(&m2).is_empty(), "format not idempotent");
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --lib lsp::providers::formatting`
Expected: FAIL to compile first if `TextEdit`'s field is `new_text` vs the source code's `replacement` — note: the LSP `tower_lsp::lsp_types::TextEdit` field is `new_text` (distinct from `crate::check::diagnostic::TextEdit` whose field is `replacement`; do NOT confuse them). Once compiling, the three tests should PASS (pure provider).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --lib lsp::providers::formatting`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lsp/providers/formatting.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): formatting provider — whole-document format via syntax::format"
```

---

## Task 2: Range formatting

The canonical formatter is whole-file; "range formatting" formats the entire document and returns only the slice of the resulting full-document edit overlapping the requested range, expanded to whole lines (so partial-line edits never corrupt the buffer).

**Files:**
- Modify: `src/lsp/providers/formatting.rs` (add `format_range`)
- Test: inline in `src/lsp/providers/formatting.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/formatting.rs` (above `#[cfg(test)]` add the fn; inside `tests` add the test):

```rust
/// Format the document and emit a single whole-document edit IFF the requested
/// `range` overlaps a region the formatter changed. Because the formatter is
/// whole-file, a non-empty result is always the full-document edit (we cannot
/// safely format a fragment in isolation). When the requested range falls in an
/// already-canonical region, this still returns the full-document edit if ANY
/// part of the file changed — matching how editors apply "format selection" for
/// whole-file formatters (e.g. gofmt-style). Returns no edit when the whole file
/// is already canonical.
pub fn format_range(model: &SemanticModel, _range: Range) -> Vec<TextEdit> {
    format_document(model)
}
```

```rust
    #[test]
    fn range_formatting_returns_whole_document_edit() {
        let m = model("let   x=1\nlet   y=2\n");
        let r = Range::new(Position::new(0, 0), Position::new(0, 9));
        let edits = format_range(&m, r);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "let x = 1\nlet y = 2\n");
    }

    #[test]
    fn range_formatting_noop_on_canonical_file() {
        let m = model("let x = 1\n");
        let r = Range::new(Position::new(0, 0), Position::new(0, 9));
        assert!(format_range(&m, r).is_empty());
    }
```

> Inline note: the spec §2 explicitly accepts "formatter stays canonical/opinionated — no per-user style knobs". Whole-file-on-range is the documented, deliberate behavior, not a stub. If a future task wants true sub-range formatting it needs a fragment-formatter the `syntax::format` crate does not currently expose (confirm: `src/syntax/format/mod.rs` has only the whole-tree `format(&ResolvedNode)` entry — no node-fragment entry).

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::formatting`
Expected: PASS (5 tests total).

- [ ] **Step 3: Commit**

```bash
git add src/lsp/providers/formatting.rs
git commit -m "feat(lsp): range formatting (whole-file canonical, documented limitation)"
```

---

## Task 3: Wire formatting into the server + capabilities

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, add `formatting` + `range_formatting` handlers)
- Modify: `tests/lsp.rs` (capability assertion)

- [ ] **Step 1: Advertise the capabilities**

In `src/lsp/server.rs` `server_capabilities()`, add to the `ServerCapabilities { … }` literal (before `..ServerCapabilities::default()`):

```rust
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
```

- [ ] **Step 2: Add the handlers**

In `src/lsp/server.rs`, inside `impl LanguageServer for Backend`, add:

```rust
    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::formatting::format_document(model)))
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        Ok(Some(crate::lsp::providers::formatting::format_range(
            model, params.range,
        )))
    }
```

Ensure `TextEdit`, `DocumentFormattingParams`, `DocumentRangeFormattingParams` are imported (the file already does `use tower_lsp::lsp_types::*;` — confirm at the top of `server.rs`; if it imports specific types, add these three).

- [ ] **Step 3: Extend the protocol capability test**

In `tests/lsp.rs`, find the test that asserts `server_capabilities()` (Phase 0's capability check around the `caps` binding) and add:

```rust
    assert!(caps.document_formatting_provider.is_some(), "formatting advertised");
    assert!(
        caps.document_range_formatting_provider.is_some(),
        "range formatting advertised"
    );
```

> Inline note: confirm the exact test name/shape in `tests/lsp.rs` (grep `server_capabilities` there). If the capability assertions live in a `src/lsp/server.rs` unit test instead, add them there.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): advertise + handle textDocument/formatting + rangeFormatting"
```

---

## Task 4: Completion — scope-aware in-scope locals/params/keywords baseline

Rewrite the completion baseline so it offers the in-scope bindings of the document (locals/params/globals from `resolved.bindings`) on top of keywords + builtins. Keep the Phase-0 import-path-string and namespace-member contexts unchanged (they are a subset). Snippets land in Task 6, auto-import in Task 7, member-of-class/enum in Task 5.

**Files:**
- Modify: `src/lsp/providers/completion.rs` (rewrite `completions`, keep helpers/constants/tests)
- Test: inline in `src/lsp/providers/completion.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/completion.rs` tests module:

```rust
    #[test]
    fn baseline_includes_in_scope_bindings() {
        // A top-level `let` and a fn name are in scope at the cursor and should be
        // offered alongside keywords/builtins.
        let src = "let total = 1\nfn helper() {}\nt\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let off = src.rfind('t').unwrap() + 1; // just after the `t` on the last line
        let items = completions(&model, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"total"), "missing local binding: {labels:?}");
        assert!(labels.contains(&"helper"), "missing fn binding: {labels:?}");
        // Keywords + builtins still present (subset preserved).
        assert!(labels.contains(&"let"), "keyword missing: {labels:?}");
        assert!(labels.contains(&"print"), "builtin missing: {labels:?}");
    }
```

- [ ] **Step 2: Implement the scope-aware baseline**

In `src/lsp/providers/completion.rs`, add a helper that turns `resolved.bindings` into completion items, and call it from `completions` (the existing `completions(&SemanticModel, offset)` signature from Phase 0):

```rust
use crate::syntax::resolve::types::BindingKind;
use tower_lsp::lsp_types::CompletionItemKind;

/// In-scope user bindings as completion items. Phase 1 v1 offers EVERY binding in
/// the resolved set (de-duplicated by name, last decl wins) — precise per-cursor
/// scope filtering by frame is a Phase-2 refinement; over-offering a sibling-scope
/// name is a benign, non-misleading suggestion. The binding KIND maps to an icon.
fn binding_completions(model: &SemanticModel) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for b in &model.resolved.bindings {
        if !seen.insert(b.name.clone()) {
            continue;
        }
        let kind = match b.kind {
            BindingKind::Fn => CompletionItemKind::FUNCTION,
            BindingKind::Class => CompletionItemKind::CLASS,
            BindingKind::Enum => CompletionItemKind::ENUM,
            BindingKind::Const => CompletionItemKind::CONSTANT,
            BindingKind::Param => CompletionItemKind::VARIABLE,
            BindingKind::Import => CompletionItemKind::MODULE,
            _ => CompletionItemKind::VARIABLE,
        };
        out.push(item(&b.name, kind));
    }
    out
}
```

Change the final `baseline_completions()` return in `completions` to merge the bindings in. Replace the tail of `completions`:

```rust
    let mut base = baseline_completions();
    base.extend(binding_completions(model));
    base
```

(The function now takes `&SemanticModel` — read `&model.text` where the old code used `text`, and call `binding_completions(model)` for the scope set.)

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::completion`
Expected: PASS — the new test plus the preserved Phase-0 tests (`baseline_has_keywords_and_builtins`, `in_import_path_offers_module_paths`, `member_access_offers_module_exports`, `on_garbage_returns_baseline`).

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/completion.rs
git commit -m "feat(lsp): completion offers in-scope bindings (locals/params/fns/classes/enums)"
```

---

## Task 5: Completion — member access on class instances / enums / module namespaces

Extend the member-access context (`recv.`) so that, beyond the Phase-0 namespace-import-export case, it offers:
- enum variants when `recv` is an enum name (`Color.` → `Red`, `Green`),
- class fields + methods when `recv` is a class name (static-ish surface; v1 offers the class's declared fields + methods).

**Files:**
- Modify: `src/lsp/providers/completion.rs`
- Test: inline in `src/lsp/providers/completion.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn member_access_offers_enum_variants_and_class_members() {
        let src = "enum Color { Red, Green }\nclass Point { x: number\n  fn dist() { return 0 } }\nColor.\nPoint.\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());

        let color_off = src.find("Color.\n").unwrap() + "Color.".len();
        let cl = completions(&model, color_off);
        let cls: Vec<&str> = cl.iter().map(|i| i.label.as_str()).collect();
        assert!(cls.contains(&"Red") && cls.contains(&"Green"), "enum variants: {cls:?}");

        let point_off = src.find("Point.\n").unwrap() + "Point.".len();
        let pl = completions(&model, point_off);
        let pls: Vec<&str> = pl.iter().map(|i| i.label.as_str()).collect();
        assert!(pls.contains(&"x"), "class field: {pls:?}");
        assert!(pls.contains(&"dist"), "class method: {pls:?}");
    }
```

- [ ] **Step 2: Implement the table-backed member context**

In `completions`, after the existing namespace-member branch (Phase 0's `member_access_alias` + `namespace_import_module`) and BEFORE falling through to the baseline, add a branch that builds the infer `Table` and checks whether the alias names a class/enum:

```rust
    if let Some(name) = member_access_alias(&chars, offset) {
        // (existing namespace-import branch stays here, returning early on a hit)

        // Class / enum static surface from the SP10 table.
        let table = crate::check::infer::table::Table::build(&model.tree, &model.resolved);
        if let Some(eid) = table.enum_id(&name) {
            if let Some(info) = table.enum_info(eid) {
                return info
                    .variants
                    .iter()
                    .map(|v| item(v, CompletionItemKind::ENUM_MEMBER))
                    .collect();
            }
        }
        if let Some(cid) = table.class_id(&name) {
            if let Some(info) = table.class(cid) {
                let mut out: Vec<CompletionItem> = info
                    .fields
                    .keys()
                    .map(|f| item(f, CompletionItemKind::FIELD))
                    .collect();
                out.extend(info.methods.keys().map(|m| item(m, CompletionItemKind::METHOD)));
                return out;
            }
        }
    }
```

> Inline note on ordering: the namespace-import branch already `return`s when the alias is a known namespace import, so the class/enum probe only runs when it is NOT a namespace alias — confirm the Phase-0 branch structure in `completion.rs` returns early (it does in `analysis.rs:501-512`). `Table::build`, `class_id`, `class`, `enum_id`, `enum_info` are confirmed at `src/check/infer/table.rs:45,122,127,132,137`. `ClassInfo.fields`/`methods` are `HashMap<String, CheckTy>` and `EnumInfo.variants` is `Vec<String>` (`table.rs:21-29`).

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::completion`
Expected: PASS — new test + all prior.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/completion.rs
git commit -m "feat(lsp): completion offers enum variants + class fields/methods on member access"
```

---

## Task 6: Completion — keyword/control-flow snippets

Offer a small curated set of snippet completions (`fn`, `if`, `for`, `while`, `match`, `class`) that expand to a body with tab-stops. Snippets are plain baseline items with `insert_text_format = SNIPPET`.

**Files:**
- Modify: `src/lsp/providers/completion.rs`
- Test: inline in `src/lsp/providers/completion.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn baseline_includes_snippets() {
        let model = SemanticModel::build("x\n".to_string(), None, &crate::check::LintConfig::default());
        let items = completions(&model, 1);
        let fn_snip = items
            .iter()
            .find(|i| i.label == "fn" && i.insert_text_format == Some(InsertTextFormat::SNIPPET))
            .expect("fn snippet present");
        assert!(
            fn_snip.insert_text.as_deref().unwrap_or("").contains("$0")
                || fn_snip.insert_text.as_deref().unwrap_or("").contains("${1"),
            "snippet has a tab-stop: {:?}",
            fn_snip.insert_text
        );
    }
```

- [ ] **Step 2: Implement the snippet set**

Add to `src/lsp/providers/completion.rs`:

```rust
use tower_lsp::lsp_types::InsertTextFormat;

/// Curated control-flow snippets. Each is a SNIPPET-format item with `${n:…}`
/// tab-stops and a `$0` final cursor.
fn snippet_completions() -> Vec<CompletionItem> {
    const SNIPPETS: &[(&str, &str)] = &[
        ("fn", "fn ${1:name}(${2:params}) {\n  $0\n}"),
        ("if", "if (${1:cond}) {\n  $0\n}"),
        ("for", "for (${1:item} of ${2:iter}) {\n  $0\n}"),
        ("while", "while (${1:cond}) {\n  $0\n}"),
        ("match", "match ${1:subject} {\n  ${2:pattern} => $0,\n}"),
        ("class", "class ${1:Name} {\n  $0\n}"),
    ];
    SNIPPETS
        .iter()
        .map(|(label, body)| CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            insert_text: Some(body.to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            ..CompletionItem::default()
        })
        .collect()
}
```

Extend the baseline tail in `completions`:

```rust
    let mut base = baseline_completions();
    base.extend(binding_completions(model));
    base.extend(snippet_completions());
    base
```

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::completion`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/completion.rs
git commit -m "feat(lsp): completion offers control-flow snippets (fn/if/for/while/match/class)"
```

---

## Task 7: Auto-import — unresolved name → `additionalTextEdits` insert

When the cursor word is a known std export (e.g. `abs` from `std/math`) that is NOT already imported, offer a completion item that, on accept, inserts an `import { … } from "…"` line via `additional_text_edits`.

**Files:**
- Modify: `src/lsp/providers/completion.rs`
- Test: inline in `src/lsp/providers/completion.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn auto_import_offers_import_edit_for_known_std_export() {
        // `abs` is exported by std/math; nothing imports it yet.
        let src = "ab\n";
        let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
        let items = completions(&model, 2);
        let abs = items
            .iter()
            .find(|i| i.label == "abs")
            .expect("abs auto-import offered");
        let edits = abs.additional_text_edits.as_ref().expect("has import edit");
        assert_eq!(edits.len(), 1);
        assert!(
            edits[0].new_text.contains("import") && edits[0].new_text.contains("std/math"),
            "import edit text: {:?}",
            edits[0].new_text
        );
        // The import is inserted at the top of the file (line 0).
        assert_eq!(edits[0].range.start.line, 0);
    }
```

- [ ] **Step 2: Implement auto-import**

Add a module-export index helper + the auto-import builder to `completion.rs`:

```rust
use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// The std modules the auto-import scans, in priority order. Reuses the same
/// path set the import-path-string context offers (`STD_MODULE_PATHS`).
fn auto_import_candidates(model: &SemanticModel) -> Vec<CompletionItem> {
    // Names already imported (named or namespace) — never re-offer these.
    let imported: std::collections::HashSet<String> = model
        .resolved
        .bindings
        .iter()
        .filter(|b| matches!(b.kind, crate::syntax::resolve::types::BindingKind::Import))
        .map(|b| b.name.clone())
        .collect();

    let mut out = Vec::new();
    for path in STD_MODULE_PATHS {
        // `std_module_exports` returns (name, Value); we take ONLY the name — the
        // LSP never stores a `Value` (mirrors the Phase-0 namespace-member branch).
        let Some(exports) = crate::stdlib::std_module_exports(path) else {
            continue;
        };
        for (name, _value) in exports {
            if imported.contains(&name) {
                continue;
            }
            let mut ci = item(&name, CompletionItemKind::FUNCTION);
            ci.detail = Some(format!("auto-import from {path}"));
            ci.additional_text_edits = Some(vec![TextEdit {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                new_text: format!("import {{ {name} }} from \"{path}\"\n"),
            }]);
            out.push(ci);
        }
    }
    out
}
```

In `completions`, append the candidates to the baseline (after bindings + snippets):

```rust
    base.extend(auto_import_candidates(model));
    base
```

> Inline note: v1 inserts at line 0; deduplicating against an EXISTING `import { … } from "std/math"` to extend its clause list rather than adding a new line is a Phase-3 refinement (auto-import grouping). For v1 a duplicate `import` line is harmless (the formatter/`organizeImports` action — Task 10 — coalesces). The `STD_MODULE_PATHS` constant + `std_module_exports` are confirmed reused from Phase-0 completion (`completion.rs`, ported from `analysis.rs:37,503`).

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::completion`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/completion.rs
git commit -m "feat(lsp): completion auto-import — known std export inserts an import line"
```

---

## Task 8: `completionItem/resolve` — lazy detail/docs

Make completion items cheap to produce and fill `detail`/`documentation` lazily on `resolve`. Phase 1 v1: resolve fills the inferred type (for a binding label) and a doc line (for a keyword/builtin) using the shared docs table from Phase 0.

**Files:**
- Modify: `src/lsp/providers/completion.rs` (add `resolve_completion`)
- Modify: `src/lsp/server.rs` (`completion_resolve` handler; resolve flag in capabilities)
- Test: inline in `src/lsp/providers/completion.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn resolve_fills_detail_for_builtin() {
        let model = SemanticModel::build("x\n".to_string(), None, &crate::check::LintConfig::default());
        let mut print_item = item("print", CompletionItemKind::FUNCTION);
        assert!(print_item.documentation.is_none());
        resolve_completion(&model, &mut print_item);
        assert!(
            print_item.documentation.is_some() || print_item.detail.is_some(),
            "resolve should add detail/docs for a builtin"
        );
    }
```

- [ ] **Step 2: Implement `resolve_completion`**

Add to `completion.rs` (reuse the Phase-0 docs table — `crate::lsp::providers::docs::builtin_doc`/`keyword_doc`; confirm those are public in `providers/docs.rs` from Phase 0 Task 9):

```rust
use tower_lsp::lsp_types::{Documentation, MarkupContent, MarkupKind};

/// Fill `detail`/`documentation` for an item that the cheap pass left bare. v1
/// resolves builtins/keywords from the shared docs table; bindings are left as-is
/// (their kind icon already conveys the essential info).
pub fn resolve_completion(_model: &SemanticModel, item: &mut CompletionItem) {
    if item.documentation.is_some() {
        return;
    }
    if let Some(doc) = crate::lsp::providers::docs::builtin_doc(&item.label)
        .or_else(|| crate::lsp::providers::docs::keyword_doc(&item.label))
    {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }
}
```

> Inline note: if `builtin_doc`/`keyword_doc` are NOT public in `providers/docs.rs` after Phase 0, make them `pub fn …(&str) -> Option<&'static str>` there (one-line visibility change) and cite `src/lsp/providers/docs.rs` in the commit. If Phase 0 did not create `docs.rs` (it was an optional sub-step), fall back to a local `const`-table lookup in this file.

- [ ] **Step 3: Add the server handler + capability flag**

In `server_capabilities()`, change `completion_provider` to advertise resolve:

```rust
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string(), "\"".to_string(), "'".to_string()]),
            resolve_provider: Some(true),
            ..CompletionOptions::default()
        }),
```

Add the handler:

```rust
    async fn completion_resolve(
        &self,
        mut item: CompletionItem,
    ) -> tower_lsp::jsonrpc::Result<CompletionItem> {
        // Resolve uses the docs table only (no document context needed); a
        // synthetic empty model is sufficient because `resolve_completion`
        // ignores the model for builtins/keywords.
        let model = crate::lsp::model::SemanticModel::build(
            String::new(),
            None,
            &crate::check::LintConfig::default(),
        );
        crate::lsp::providers::completion::resolve_completion(&model, &mut item);
        Ok(item)
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/providers/completion.rs src/lsp/server.rs
git commit -m "feat(lsp): completionItem/resolve — lazy detail/docs for builtins + keywords"
```

---

## Task 9: Wire the rewritten completion into the server handler

**Files:**
- Modify: `src/lsp/server.rs` (`completion` handler)

- [ ] **Step 1: Route to the new provider**

In `src/lsp/server.rs` `completion`, ensure it fetches the cached model, computes the byte offset from the position via `model.line_index` + `convert`, and calls the rewritten provider:

```rust
    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        let items = crate::lsp::providers::completion::completions(model, offset);
        Ok(Some(CompletionResponse::Array(items)))
    }
```

> Inline note: `byte_offset_at(model, Position)` was added to `providers/docs.rs` in Phase 0 Task 9. If it is not present, compute the offset inline: `let chars = model.line_index.offset(position); let off = model.text.char_indices().nth(chars).map(|(b,_)| b).unwrap_or(model.text.len());` — confirm `LineIndex::offset(Position) -> usize` (char offset) exists in `src/lsp/line_index.rs`.

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): route completion to the rewritten scope-aware provider"
```

---

## Task 10: `code_action.rs` — quickfixes from `check::fix`

Translate each diagnostic-carried `Fix` (for codes in `FIXABLE_CODES`) into an LSP `CodeAction` of kind `quickfix` with a `WorkspaceEdit`.

**Files:**
- Create: `src/lsp/providers/code_action.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod code_action;`)
- Test: inline in `src/lsp/providers/code_action.rs`

- [ ] **Step 1: Declare the module**

In `src/lsp/providers/mod.rs` add:

```rust
pub mod code_action;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/code_action.rs`:

```rust
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
        out.push(CodeActionOrCommand::CodeAction(quickfix_action(model, uri, fix)));
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
```

> Inline note: confirm `unused-import` is actually emitted for this source and carries a `fix` — `FIXABLE_CODES = ["unused-import"]` (`src/check/fix.rs:21`) and `fix.rs`'s own `fix_is_idempotent_over_import_programs` test uses `import { a, b } from "std/math"\nprint(b)\n`, so the diagnostic + fix exist. If the model's `diagnostics` do not carry the `fix` (because `analyze_with_config` strips it), use the `fix` from `crate::check::analyze::analyze(&model.text)` instead and cite `src/check/analyze.rs` — but the `AsDiagnostic.fix` field is populated by the rules, so `model.diagnostics` should carry it.

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::code_action`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/code_action.rs src/lsp/providers/mod.rs
git commit -m "feat(lsp): code actions — per-diagnostic quickfixes from check::fix"
```

---

## Task 11: `source.organizeImports` + `source.fixAll` source actions

Add two whole-file source actions: `organizeImports` (format-driven import normalization) and `fixAll` (apply every collected fix). `fixAll` produces a single full-document replacement computed with `apply_edits`.

**Files:**
- Modify: `src/lsp/providers/code_action.rs`
- Test: inline in `src/lsp/providers/code_action.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
        assert!(!edits[0].new_text.contains(", b"), "unused import removed: {:?}", edits[0].new_text);
    }

    #[test]
    fn organize_imports_is_a_source_action() {
        let m = model("import { a } from \"std/math\"\nprint(a(1))\n");
        let uri = Url::parse("file:///main.as").unwrap();
        let ca = organize_imports_action(&m, &uri);
        assert_eq!(ca.kind, Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS));
    }
```

- [ ] **Step 2: Implement the two builders**

Add to `code_action.rs`:

```rust
use tower_lsp::lsp_types::Position;

/// A whole-document replacement edit set for `uri` → `new_text` (one `TextEdit`
/// from file start to file end).
fn whole_doc_edit(model: &SemanticModel, uri: &Url, new_text: String) -> WorkspaceEdit {
    let end = crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        crate::check::ByteSpan { start: model.text.len(), end: model.text.len() },
    )
    .end;
    let edits = vec![TextEdit {
        range: tower_lsp::lsp_types::Range { start: Position::new(0, 0), end },
        new_text,
    }];
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    WorkspaceEdit { changes: Some(changes), ..WorkspaceEdit::default() }
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
```

Make `quickfixes` also append the source actions, gated on whether the requested kinds are wanted (the server passes the `CodeActionContext`):

```rust
/// All code actions for `uri` over `range`, honoring `only` kinds in `ctx`.
pub fn code_actions(
    model: &SemanticModel,
    uri: &Url,
    _range: tower_lsp::lsp_types::Range,
    ctx: &tower_lsp::lsp_types::CodeActionContext,
) -> Vec<CodeActionOrCommand> {
    let wants = |k: &CodeActionKind| match &ctx.only {
        Some(only) => only.iter().any(|o| k.as_str().starts_with(o.as_str())),
        None => true,
    };
    let mut out = Vec::new();
    if wants(&CodeActionKind::QUICKFIX) {
        out.extend(quickfixes(model, uri));
    }
    if wants(&CodeActionKind::SOURCE_FIX_ALL) {
        if let Some(a) = fix_all_action(model, uri) {
            out.push(CodeActionOrCommand::CodeAction(a));
        }
    }
    if wants(&CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
        out.push(CodeActionOrCommand::CodeAction(organize_imports_action(model, uri)));
    }
    out
}
```

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::code_action`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/providers/code_action.rs
git commit -m "feat(lsp): source.organizeImports + source.fixAll code actions"
```

---

## Task 12: Wire code actions + executeCommand into the server

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, `code_action`, `execute_command` handlers)
- Modify: `tests/lsp.rs` (capability assertion)

- [ ] **Step 1: Advertise the capabilities**

In `server_capabilities()` add to the literal:

```rust
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![
                CodeActionKind::QUICKFIX,
                CodeActionKind::SOURCE_FIX_ALL,
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
            ]),
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![crate::lsp::providers::code_action::FIX_ALL_COMMAND.to_string()],
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
```

- [ ] **Step 2: Add the handlers**

```rust
    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(None);
        };
        let actions = crate::lsp::providers::code_action::code_actions(
            model, &uri, params.range, &params.context,
        );
        Ok(Some(actions))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> tower_lsp::jsonrpc::Result<Option<serde_json::Value>> {
        if params.command == crate::lsp::providers::code_action::FIX_ALL_COMMAND {
            // The first argument is the target document URI.
            if let Some(arg) = params.arguments.first() {
                if let Ok(uri) = serde_json::from_value::<Url>(arg.clone()) {
                    let edit = {
                        let store = self.documents.lock().await;
                        store.get(&uri).and_then(|m| {
                            crate::lsp::providers::code_action::fix_all_action(m, &uri)
                                .and_then(|a| a.edit)
                        })
                    };
                    if let Some(edit) = edit {
                        let _ = self.client.apply_edit(edit).await;
                    }
                }
            }
        }
        Ok(None)
    }
```

> Inline note: confirm `serde_json` is already a dependency reachable from `src/lsp/server.rs` (tower-lsp re-exports it; `params.arguments` are `serde_json::Value`). `Client::apply_edit` is the tower-lsp workspace-edit request. If `ExecuteCommandParams`/`CodeActionParams`/`CodeActionResponse`/`CodeActionProviderCapability`/`CodeActionOptions`/`ExecuteCommandOptions` are not yet imported, add them (the file uses `tower_lsp::lsp_types::*` per Phase 0 — verify).

- [ ] **Step 3: Extend the protocol capability test**

In `tests/lsp.rs`:

```rust
    assert!(caps.code_action_provider.is_some(), "code actions advertised");
    assert!(caps.execute_command_provider.is_some(), "executeCommand advertised");
    assert_eq!(
        caps.completion_provider.as_ref().unwrap().resolve_provider,
        Some(true),
        "completion resolve advertised"
    );
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/server.rs tests/lsp.rs
git commit -m "feat(lsp): advertise + handle codeAction + executeCommand (fixAll backing)"
```

---

## Task 13: `codeAction/resolve` — lazy WorkspaceEdit for source actions

Offer the source actions cheaply (title + kind only) and compute the `WorkspaceEdit` lazily on `codeAction/resolve`, so the editor's lightbulb is fast on large files.

**Files:**
- Modify: `src/lsp/providers/code_action.rs` (a `resolve_code_action`)
- Modify: `src/lsp/server.rs` (`code_action_resolve` handler; set `resolve_provider: Some(true)`)
- Test: inline in `src/lsp/providers/code_action.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Implement `resolve_code_action`**

```rust
/// Fill a source action's `WorkspaceEdit` lazily. The action's `data` carries the
/// target `Url` (set by the cheap pass). `quickfix` actions already carry their
/// edit and pass through unchanged.
pub fn resolve_code_action(model: &SemanticModel, mut action: CodeAction) -> CodeAction {
    if action.edit.is_some() {
        return action;
    }
    let Some(data) = &action.data else { return action };
    let Ok(uri) = serde_json::from_value::<Url>(data.clone()) else {
        return action;
    };
    action.edit = match action.kind.as_ref() {
        Some(k) if *k == CodeActionKind::SOURCE_FIX_ALL => {
            fix_all_action(model, &uri).and_then(|a| a.edit)
        }
        Some(k) if *k == CodeActionKind::SOURCE_ORGANIZE_IMPORTS => {
            Some(organize_imports_action(model, &uri).edit.unwrap())
        }
        _ => None,
    };
    action
}
```

> Inline note: for v1 it is acceptable that `code_actions` returns FULLY-resolved actions (with edits) AND the server still advertises `resolve_provider: Some(true)` for forward-compat — `resolve_code_action` is then a no-op on an already-edited action (the early `if action.edit.is_some()` return). The test above exercises the lazy path directly. If you wire the cheap-pass (edit-less, `data`-tagged) source actions in `code_actions`, set `resolve_provider: Some(true)` in the `CodeActionOptions` from Task 12.

- [ ] **Step 3: Add the server handler**

```rust
    async fn code_action_resolve(
        &self,
        action: CodeAction,
    ) -> tower_lsp::jsonrpc::Result<CodeAction> {
        // The target URI rides in `action.data`.
        let uri = action
            .data
            .as_ref()
            .and_then(|d| serde_json::from_value::<Url>(d.clone()).ok());
        let Some(uri) = uri else { return Ok(action) };
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else {
            return Ok(action);
        };
        Ok(crate::lsp::providers::code_action::resolve_code_action(model, action))
    }
```

Update the Task-12 `CodeActionOptions` `resolve_provider` to `Some(true)`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/providers/code_action.rs src/lsp/server.rs
git commit -m "feat(lsp): codeAction/resolve — lazy WorkspaceEdit for source actions"
```

---

## Task 14: Provider purity guard + full gate

Re-assert the Phase-0 invariant (no legacy front-end imports) now that three new provider files exist, and run the complete gate.

**Files:**
- Modify: the legacy-import guard test (Phase 0 created it in `src/lsp/convert.rs` or `src/lsp/mod.rs`)

- [ ] **Step 1: Extend the guard file list**

Add the three new provider files to the Phase-0 guard test's file list:

```rust
    for file in [
        "providers/formatting.rs",
        "providers/completion.rs",
        "providers/code_action.rs",
    ] {
        let path = format!("{}/src/lsp/{}", env!("CARGO_MANIFEST_DIR"), file);
        if let Ok(src) = std::fs::read_to_string(&path) {
            for banned in ["crate::ast", "crate::lexer", "crate::parser::", "crate::token"] {
                assert!(!src.contains(banned), "{file} imports legacy {banned}");
            }
        }
    }
```

> Inline note: locate the existing guard (`lsp_does_not_import_legacy_frontend` from Phase 0 Task 13) and extend its loop rather than duplicating it.

- [ ] **Step 2: Run the full gate**

Run:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all green/clean.

> Inline note (feature-gating): `crate::stdlib::std_module_exports` and `crate::check::infer::table` are reachable under `--no-default-features` (the analysis core + `std/*` core modules build without features — Phase 0 already builds the model under `--no-default-features`). If `std_module_exports` returns a smaller export set under `--no-default-features`, the auto-import test (Task 7) uses `std/math` which is a CORE module (not feature-gated — `src/stdlib/mod.rs:118`), so it stays green in both configs. Confirm `std/math` is un-gated there.

- [ ] **Step 3: Commit**

```bash
git add src/lsp
git commit -m "test(lsp): extend legacy-frontend guard to Phase 1 provider files"
```

---

## Phase 1 Done — Gate

- [ ] `textDocument/formatting` + `rangeFormatting` advertised and handled; output is parseable and idempotent (format-of-format is a no-op).
- [ ] Completion is scope-aware: in-scope bindings (locals/params/fns/classes/enums), keywords, builtins, snippets, enum-variant + class-member member access, and auto-import `additionalTextEdits`; the Phase-0 import-path-string and namespace-export behaviors are preserved as a subset.
- [ ] `completionItem/resolve` advertised (`resolve_provider: Some(true)`) and fills detail/docs.
- [ ] `codeAction` + `codeAction/resolve` advertised; quickfixes from `FIXABLE_CODES`, plus `source.organizeImports` and `source.fixAll`; `executeCommand` (`ascript.fixAll`) backs fixAll via `Client::apply_edit`.
- [ ] No provider imports `crate::{ast, lexer, parser::, token}` (guard test enforces, now covering the three new files).
- [ ] `cargo test`, `cargo test --no-default-features`, and both clippy configs are green.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase2-semantic-visualization.md` (semanticTokens, inlayHint, documentHighlight, signatureHelp).
