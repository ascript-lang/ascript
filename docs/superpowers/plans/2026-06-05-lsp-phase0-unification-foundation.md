# LSP Phase 0 — Unification Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the LSP onto a single cached `SemanticModel` (CST + resolver + infer + config-aware diagnostics), add incremental sync, refactor the workspace index to reuse cached models, and **delete the legacy `crate::{ast,lexer,parser}` front-end from the LSP** — with the current 8 capabilities behaving identically.

**Architecture:** A per-document `SemanticModel` is built once per version from the new CST front-end (`syntax::parser::parse` → `tree_builder::build_tree` → `resolve::resolve`), plus `check::analyze_with_config` diagnostics and `syntax::lex` tokens. Every provider becomes a pure `fn(&SemanticModel, …)`. The `Backend` swaps its `HashMap<Url,String>` text store for a `HashMap<Url, SemanticModel>` cache. The legacy single-file providers in `analysis.rs` are ported onto the model and their legacy imports removed.

**Tech Stack:** Rust, `tower-lsp`, `cstree` (red/green CST), the existing `src/syntax/` + `src/check/` crates.

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §3 (architecture), §6 (Phase 0).
- Current LSP: `src/lsp/{server.rs,analysis.rs,workspace.rs,line_index.rs}`.
- Front-end API: `src/syntax/mod.rs` (`parse_to_tree`, `resolve_source`, `lex`), `src/syntax/parser.rs` (`parse`, `Parse`), `src/syntax/tree_builder.rs` (`build_tree`), `src/syntax/resolve/mod.rs` (`resolve`).
- Diagnostics: `src/check/analyze.rs` (`analyze`, `analyze_with_config`), `src/check/mod.rs` (`LintConfig`, `AsDiagnostic`, `ByteSpan`).
- Config discovery (currently CLI-only): `src/lint_config_toml.rs` (`discover`, `config_for_file`).

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit tests) and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

---

## File Structure

- Create `src/lsp/convert.rs` — byte↔`Position`/`Range`, `ByteSpan`→`Range`. The single coordinate-conversion home.
- Create `src/lsp/model.rs` — `SemanticModel` (built artifacts) + `DocumentStore` (the per-URL cache).
- Create `src/lsp/providers/mod.rs` + `src/lsp/providers/{symbols,hover,navigation,completion}.rs` — providers ported off legacy onto `&SemanticModel`.
- Create `src/check/config_toml.rs` — relocated lint-config discovery, now library-visible.
- Modify `src/lsp/mod.rs` — declare the new modules.
- Modify `src/lsp/server.rs` — cache + incremental sync + config-aware publish.
- Modify `src/lsp/analysis.rs` — strip ported providers + legacy imports; keep only shared conversion shims it still owns until they move.
- Modify `src/check/mod.rs`, `src/main.rs` — re-export/relocate config discovery.
- Modify `src/lsp/workspace.rs` — accept a borrowed cached model where it currently re-parses.

---

## Task 1: `convert.rs` — coordinate conversion

**Files:**
- Create: `src/lsp/convert.rs`
- Modify: `src/lsp/mod.rs` (add `pub mod convert;`)
- Test: inline `#[cfg(test)]` in `src/lsp/convert.rs`

- [ ] **Step 1: Add the module declaration**

In `src/lsp/mod.rs`, add alongside the existing `pub mod` lines:

```rust
pub mod convert;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/convert.rs`:

```rust
//! Coordinate conversion for the LSP: the analysis core speaks BYTE offsets,
//! LSP speaks UTF-16 line/character `Position`s. All conversion lives here.

use crate::check::ByteSpan;
use crate::lsp::line_index::LineIndex;
use tower_lsp::lsp_types::{Position, Range};

/// Convert a byte offset to a char offset (the char-based `LineIndex` then maps
/// char→`Position`). Clamps to the largest char boundary `<= byte` so a
/// mid-codepoint byte never panics.
pub fn byte_to_char(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Convert a byte-offset `ByteSpan` to an LSP `Range`.
pub fn byte_span_to_range(src: &str, index: &LineIndex, span: ByteSpan) -> Range {
    Range {
        start: index.position(byte_to_char(src, span.start)),
        end: index.position(byte_to_char(src, span.end)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_char_handles_multibyte() {
        // "héllo": 'é' is 2 bytes, so byte 3 is the 'l' at char index 2.
        let src = "héllo";
        assert_eq!(byte_to_char(src, 0), 0);
        assert_eq!(byte_to_char(src, 3), 2);
        // A mid-codepoint byte (inside 'é', byte 2) clamps down to char 1.
        assert_eq!(byte_to_char(src, 2), 1);
        // Out-of-range clamps to the end.
        assert_eq!(byte_to_char(src, 999), 5);
    }

    #[test]
    fn byte_span_to_range_maps_endpoints() {
        let src = "let x = 1\nprint(x)\n";
        let index = LineIndex::new(src);
        // "print" starts at byte 10 (line 1, char 0), ends at byte 15.
        let r = byte_span_to_range(src, &index, ByteSpan { start: 10, end: 15 });
        assert_eq!(r.start, Position::new(1, 0));
        assert_eq!(r.end, Position::new(1, 5));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib lsp::convert`
Expected: FAIL to COMPILE first (e.g. `cannot find ByteSpan` if not re-exported) — if `ByteSpan` is not at `crate::check::ByteSpan`, confirm it is (`src/check/mod.rs` line 10 re-exports it). Once compiling, tests should PASS (this task is pure). If a compile error about `ByteSpan` import path appears, that is the expected initial failure.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib lsp::convert`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lsp/convert.rs src/lsp/mod.rs
git commit -m "feat(lsp): convert.rs — central byte<->Range coordinate conversion"
```

---

## Task 2: `SemanticModel` — build the cached artifacts

**Files:**
- Create: `src/lsp/model.rs`
- Modify: `src/lsp/mod.rs` (add `pub mod model;`)
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 1: Add the module declaration**

In `src/lsp/mod.rs` add:

```rust
pub mod model;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/model.rs`:

```rust
//! The per-document semantic model: the single parsed/resolved artifact every
//! LSP provider reads. Built once per document version. Holds only owned,
//! `Send + Sync` data (CST + resolver result + diagnostics + tokens) — never a
//! `Value`/`Rc`/`RefCell`, so the backend stays `Send + Sync`.

use crate::check::{AsDiagnostic, LintConfig};
use crate::lsp::line_index::LineIndex;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::lexer::LexToken;
use crate::syntax::resolve::types::ResolveResult;

/// Everything the LSP knows about one document at one version.
pub struct SemanticModel {
    pub text: String,
    pub version: Option<i32>,
    pub tree: ResolvedNode,
    pub resolved: ResolveResult,
    pub diagnostics: Vec<AsDiagnostic>,
    pub tokens: Vec<LexToken>,
    pub line_index: LineIndex,
}

impl SemanticModel {
    /// Build the model for `text`, applying `config` to diagnostic severities.
    pub fn build(text: String, version: Option<i32>, config: &LintConfig) -> Self {
        let parsed = crate::syntax::parser::parse(&text);
        let tree = crate::syntax::tree_builder::build_tree(parsed);
        let resolved = crate::syntax::resolve::resolve(&tree);
        let diagnostics = crate::check::analyze::analyze_with_config(&text, config).diagnostics;
        let tokens: Vec<LexToken> = crate::syntax::lex(&text).collect();
        let line_index = LineIndex::new(&text);
        SemanticModel {
            text,
            version,
            tree,
            resolved,
            diagnostics,
            tokens,
            line_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_clean_program_has_no_diagnostics() {
        let m = SemanticModel::build("let x = 1\nprint(x)\n".to_string(), Some(1), &LintConfig::default());
        assert!(m.diagnostics.is_empty(), "got {:?}", m.diagnostics);
        // The model exposes a usable tree + tokens.
        assert!(!m.tokens.is_empty());
    }

    #[test]
    fn build_matches_analyze_with_config() {
        let src = "let = 1\nlet = 2\n";
        let m = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let direct = crate::check::analyze::analyze_with_config(src, &LintConfig::default()).diagnostics;
        assert_eq!(m.diagnostics, direct);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails/compiles**

Run: `cargo test --lib lsp::model`
Expected: First failure is a compile error if `crate::syntax::lex` does not return an iterator of `LexToken` — verify the signature in `src/syntax/lexer.rs` (`analyze.rs` already does `for t in crate::syntax::lex(src)`). If `lex` yields `LexToken` by reference, change the collect to `.cloned().collect()`. Fix until it compiles.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib lsp::model`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/lsp/model.rs src/lsp/mod.rs
git commit -m "feat(lsp): SemanticModel — single cached CST+resolver+diagnostics+tokens artifact"
```

---

## Task 3: Relocate lint-config discovery into the library

The LSP cannot call `src/lint_config_toml.rs` because that is a `main.rs`-only module. Move it into the library so both the CLI and the LSP discover the same `ascript.toml [lint]`.

**Files:**
- Create: `src/check/config_toml.rs` (moved content of `src/lint_config_toml.rs`)
- Modify: `src/check/mod.rs` (add `pub mod config_toml;`)
- Modify: `src/main.rs` (drop `mod lint_config_toml;`, call `ascript::check::config_toml::config_for_file`)
- Delete: `src/lint_config_toml.rs`
- Test: inline in `src/check/config_toml.rs`

- [ ] **Step 1: Move the file**

```bash
git mv src/lint_config_toml.rs src/check/config_toml.rs
```

- [ ] **Step 2: Register it in the library and fix the CLI**

In `src/check/mod.rs` add:

```rust
pub mod config_toml;
```

In `src/main.rs`, remove the line `mod lint_config_toml;` and replace every `lint_config_toml::config_for_file(` call with `ascript::check::config_toml::config_for_file(`. (Grep: `rg 'lint_config_toml' src/main.rs` — update each hit. If `config_toml.rs` references `crate::` paths that were valid in the binary but not the library, repoint them to `crate::check::…` / `crate::…` as the compiler directs.)

- [ ] **Step 3: Write the failing test**

Append to `src/check/config_toml.rs`:

```rust
#[cfg(test)]
mod relocation_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn discover_finds_nearest_ascript_toml() {
        let dir = tempfile::tempdir().unwrap();
        let toml = dir.path().join("ascript.toml");
        let mut f = std::fs::File::create(&toml).unwrap();
        writeln!(f, "[lint]\nunused-binding = \"allow\"").unwrap();
        let src = dir.path().join("main.as");
        std::fs::File::create(&src).unwrap();
        assert_eq!(discover(&src).as_deref(), Some(toml.as_path()));
    }
}
```

- [ ] **Step 4: Run to verify it fails, then compiles green**

Run: `cargo test --lib check::config_toml && cargo build`
Expected: the binary build fails first if any `main.rs` reference was missed — fix each. Then PASS.

- [ ] **Step 5: Verify clippy in both configs**

Run: `cargo clippy --all-targets && cargo clippy --no-default-features --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(check): relocate ascript.toml lint discovery into the library (LSP can reuse)"
```

---

## Task 4: `DocumentStore` — per-URL model cache + config discovery

**Files:**
- Modify: `src/lsp/model.rs` (add `DocumentStore`)
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/model.rs`:

```rust
use std::collections::HashMap;
use std::path::Path;
use tower_lsp::lsp_types::Url;

/// The per-document model cache. Keyed by `Url`; holds at most one model per open
/// document. Rebuilds on insert (full or post-incremental text).
#[derive(Default)]
pub struct DocumentStore {
    models: HashMap<Url, SemanticModel>,
}

impl DocumentStore {
    pub fn new() -> Self {
        DocumentStore { models: HashMap::new() }
    }

    /// Build and store the model for `uri` at `text`/`version`. The lint config is
    /// discovered from the document's filesystem path (nearest `ascript.toml`); a
    /// non-file URL uses the default config.
    pub fn set(&mut self, uri: Url, text: String, version: Option<i32>) {
        let config = config_for_uri(&uri);
        let model = SemanticModel::build(text, version, &config);
        self.models.insert(uri, model);
    }

    pub fn get(&self, uri: &Url) -> Option<&SemanticModel> {
        self.models.get(uri)
    }

    pub fn remove(&mut self, uri: &Url) {
        self.models.remove(uri);
    }
}

/// Discover the lint config for a document URL (nearest `ascript.toml [lint]`).
fn config_for_uri(uri: &Url) -> LintConfig {
    if let Ok(path) = uri.to_file_path() {
        return config_for_path(&path);
    }
    LintConfig::default()
}

/// Discover the lint config for a filesystem path.
pub fn config_for_path(path: &Path) -> LintConfig {
    crate::check::config_toml::config_for_file(path).unwrap_or_default()
}

#[cfg(test)]
mod store_tests {
    use super::*;

    #[test]
    fn set_get_remove_roundtrip() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        store.set(uri.clone(), "let x = 1\n".to_string(), Some(1));
        assert!(store.get(&uri).is_some());
        assert_eq!(store.get(&uri).unwrap().version, Some(1));
        store.remove(&uri);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn set_rebuilds_on_new_version() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        store.set(uri.clone(), "let x = 1\n".to_string(), Some(1));
        store.set(uri.clone(), "let = bad\n".to_string(), Some(2));
        let m = store.get(&uri).unwrap();
        assert_eq!(m.version, Some(2));
        assert!(!m.diagnostics.is_empty(), "v2 has a syntax error");
    }
}
```

Confirm `config_for_file` returns `Option<LintConfig>`; if it returns `Result`, adapt `.unwrap_or_default()` accordingly (check `src/check/config_toml.rs`).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib lsp::model::store_tests`
Expected: FAIL to compile if `config_for_file`'s return type differs — adapt `config_for_path` to its real signature.

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::model`
Expected: PASS (all model tests).

- [ ] **Step 4: Commit**

```bash
git add src/lsp/model.rs
git commit -m "feat(lsp): DocumentStore — per-URL model cache with ascript.toml-aware config"
```

---

## Task 5: Diagnostics-from-model helper

Provide the single function the server uses to turn a model into LSP diagnostics (replacing `analysis::diagnostics`, which re-parses and uses the default config).

**Files:**
- Modify: `src/lsp/model.rs`
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/model.rs`:

```rust
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

impl SemanticModel {
    /// The model's diagnostics as LSP `Diagnostic`s (severity + code + range).
    pub fn lsp_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .map(|d| Diagnostic {
                range: crate::lsp::convert::byte_span_to_range(&self.text, &self.line_index, d.range),
                severity: Some(match d.severity {
                    crate::check::Severity::Error => DiagnosticSeverity::ERROR,
                    crate::check::Severity::Warning => DiagnosticSeverity::WARNING,
                    crate::check::Severity::Info => DiagnosticSeverity::INFORMATION,
                    crate::check::Severity::Hint => DiagnosticSeverity::HINT,
                }),
                code: Some(NumberOrString::String(d.code.clone())),
                source: Some("ascript".to_string()),
                message: d.message.clone(),
                ..Diagnostic::default()
            })
            .collect()
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    #[test]
    fn lsp_diagnostics_carry_code_and_severity() {
        let m = SemanticModel::build("let = 1\n".to_string(), None, &LintConfig::default());
        let ds = m.lsp_diagnostics();
        assert!(!ds.is_empty());
        let d = &ds[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert!(matches!(&d.code, Some(NumberOrString::String(c)) if c == "syntax-error"));
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::model::diag_tests`
Expected: PASS after compile (uses `convert` from Task 1).

- [ ] **Step 3: Commit**

```bash
git add src/lsp/model.rs
git commit -m "feat(lsp): SemanticModel::lsp_diagnostics — config-aware diagnostics off the model"
```

---

## Task 6: Wire `Backend` to the model cache + config-aware publish

**Files:**
- Modify: `src/lsp/server.rs:18-63` (Backend fields + `analyze_and_publish`)
- Test: extend the protocol smoke test (`tests/lsp.rs`) — assert diagnostics still publish.

- [ ] **Step 1: Swap the text store for the model cache**

In `src/lsp/server.rs`, change the `documents` field and `Backend::new`:

```rust
use crate::lsp::model::DocumentStore;
// remove: use std::collections::HashMap;  (if now unused)

pub struct Backend {
    client: Client,
    documents: Mutex<DocumentStore>,
    index: RwLock<WorkspaceIndex>,
    roots: RwLock<Vec<PathBuf>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            documents: Mutex::new(DocumentStore::new()),
            index: RwLock::new(WorkspaceIndex::new()),
            roots: RwLock::new(Vec::new()),
        }
    }
```

- [ ] **Step 2: Rewrite `analyze_and_publish` to use the cached model**

Replace the body of `analyze_and_publish`:

```rust
    async fn analyze_and_publish(&self, uri: Url, text: String, version: Option<i32>) {
        let mut diags;
        {
            let mut store = self.documents.lock().await;
            store.set(uri.clone(), text.clone(), version);
            diags = store.get(&uri).map(|m| m.lsp_diagnostics()).unwrap_or_default();
        }
        // Index-backed cross-file arity (cannot be computed single-file).
        if let Some(path) = url_to_canon(&uri) {
            if let Ok(idx) = self.index.read() {
                for d in idx.file_module_arity(&path, &text) {
                    diags.push(analysis::byte_diagnostic_to_lsp(&text, &d));
                }
            }
        }
        self.client.publish_diagnostics(uri, diags, version).await;
    }
```

Update the other readers of `self.documents` (`document_symbol`, `hover`, `completion`, `goto_definition`, `references`, `rename`, `prepare_rename`) to fetch text via `store.get(&uri).map(|m| m.text.as_str())` instead of the old `docs.get(&uri)`. (Grep: `rg 'self.documents.lock' src/lsp/server.rs`.) These handlers keep calling the existing `analysis::*` functions for now — Tasks 8–11 port them.

- [ ] **Step 3: Run the protocol + unit tests**

Run: `cargo test --test lsp && cargo test --lib lsp`
Expected: PASS — diagnostics still flow; capability set unchanged.

- [ ] **Step 4: Commit**

```bash
git add src/lsp/server.rs
git commit -m "feat(lsp): Backend uses the SemanticModel cache + config-aware diagnostics"
```

---

## Task 7: Incremental text sync

**Files:**
- Modify: `src/lsp/server.rs` (`server_capabilities`, `did_change`)
- Modify: `src/lsp/model.rs` (a pure `apply_changes` text edit + differential test)
- Test: inline in `src/lsp/model.rs`

- [ ] **Step 1: Write the failing differential test**

Append to `src/lsp/model.rs`:

```rust
use tower_lsp::lsp_types::TextDocumentContentChangeEvent;

/// Apply LSP incremental content changes to `text` in order, returning the new
/// text. A change with `range == None` replaces the whole document. Ranges are
/// UTF-16 line/char positions resolved against the CURRENT text before each edit.
pub fn apply_changes(text: &str, changes: &[TextDocumentContentChangeEvent]) -> String {
    let mut out = text.to_string();
    for change in changes {
        match change.range {
            None => out = change.text.clone(),
            Some(range) => {
                let index = LineIndex::new(&out);
                let start = char_offset(&out, index.offset(range.start));
                let end = char_offset(&out, index.offset(range.end));
                out.replace_range(start..end, &change.text);
            }
        }
    }
    out
}

/// Byte offset of char offset `chars` in `s`.
fn char_offset(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use tower_lsp::lsp_types::{Position, Range};

    #[test]
    fn incremental_edits_equal_full_reparse() {
        // Start, then apply a ranged insert, and assert the text equals what a
        // client would have sent as a full document.
        let start = "let x = 1\nprint(x)\n";
        let change = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))), // the "1"
            range_length: None,
            text: "42".to_string(),
        };
        let got = apply_changes(start, &[change]);
        assert_eq!(got, "let x = 42\nprint(x)\n");
        // And a model built from the incremental result equals one built from the
        // equivalent full text (diagnostics identical).
        let inc = SemanticModel::build(got.clone(), Some(2), &LintConfig::default());
        let full = SemanticModel::build("let x = 42\nprint(x)\n".to_string(), Some(2), &LintConfig::default());
        assert_eq!(inc.diagnostics, full.diagnostics);
    }

    #[test]
    fn full_replace_change_replaces() {
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "new".to_string(),
        };
        assert_eq!(apply_changes("old", &[change]), "new");
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::model::sync_tests`
Expected: PASS once it compiles.

- [ ] **Step 3: Advertise incremental sync + apply it in `did_change`**

In `src/lsp/server.rs` `server_capabilities`, change:

```rust
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::INCREMENTAL)),
```

Rewrite `did_change` to apply the changes against the cached text, then rebuild:

```rust
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = Some(params.text_document.version);
        let new_text = {
            let store = self.documents.lock().await;
            let base = store.get(&uri).map(|m| m.text.clone()).unwrap_or_default();
            crate::lsp::model::apply_changes(&base, &params.content_changes)
        };
        self.reindex_uri(&uri, &new_text);
        self.analyze_and_publish(uri, new_text, version).await;
    }
```

- [ ] **Step 4: Run the protocol test**

Run: `cargo test --test lsp && cargo test --lib lsp`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lsp/server.rs src/lsp/model.rs
git commit -m "feat(lsp): incremental text sync (ranged edits) with full-reparse differential"
```

---

## Task 8: Port `documentSymbol` onto the CST model

**Files:**
- Create: `src/lsp/providers/mod.rs`, `src/lsp/providers/symbols.rs`
- Modify: `src/lsp/mod.rs` (add `pub mod providers;`), `src/lsp/server.rs` (`document_symbol` handler)
- Test: inline in `src/lsp/providers/symbols.rs`

- [ ] **Step 1: Declare the providers module**

In `src/lsp/mod.rs` add `pub mod providers;`. Create `src/lsp/providers/mod.rs`:

```rust
//! LSP providers. Each is a pure `fn(&SemanticModel, …) -> …` over the cached
//! model — no provider re-parses or touches the legacy `crate::{ast,lexer,parser}`.
pub mod symbols;
```

- [ ] **Step 2: Write the failing test**

Create `src/lsp/providers/symbols.rs`. Walk the CST top-level declarations (mirror the kinds in `src/lsp/workspace.rs`'s `index_tree`, which already classifies `FnDecl`/`ClassDecl`/`EnumDecl`/`LetDecl`/`ImportStmt`). Use `crate::syntax::resolve::ident_text` for names and `code_range`-style byte spans from `text_range()`.

```rust
//! `textDocument/documentSymbol` over the CST.

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};

/// Top-level document symbols (functions, classes + their methods/fields, enums +
/// variants, lets/consts). Nesting: class → methods/fields, enum → variants.
#[allow(deprecated)] // DocumentSymbol::deprecated field
pub fn document_symbols(model: &SemanticModel) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for node in model.tree.children() {
        if let Some(sym) = symbol_for(model, &node) {
            out.push(sym);
        }
    }
    out
}

#[allow(deprecated)]
fn symbol_for(model: &SemanticModel, node: &crate::syntax::cst::ResolvedNode) -> Option<DocumentSymbol> {
    let (kind, _) = match node.kind() {
        SyntaxKind::FnDecl => (SymbolKind::FUNCTION, ()),
        SyntaxKind::ClassDecl => (SymbolKind::CLASS, ()),
        SyntaxKind::EnumDecl => (SymbolKind::ENUM, ()),
        SyntaxKind::LetDecl => (SymbolKind::VARIABLE, ()),
        _ => return None,
    };
    let name = crate::syntax::resolve::ident_text(node)?;
    let range = crate::lsp::convert::byte_span_to_range(
        &model.text, &model.line_index, crate::check::ByteSpan::from(node.text_range()),
    );
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn lists_top_level_decls() {
        let syms = document_symbols(&model("fn foo() {}\nclass C {}\nenum E { A, B }\nlet v = 1\n"));
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "{names:?}");
        assert!(names.contains(&"C"), "{names:?}");
        assert!(names.contains(&"E"), "{names:?}");
        assert!(names.contains(&"v"), "{names:?}");
    }
}
```

Adapt `node.kind()` matches and `ident_text` to the real `ResolvedNode` API — confirm against `src/lsp/workspace.rs` (`index_tree`, `decl_kind`, `name_range_of`), which already does exactly this walk and is the authoritative pattern to copy. Add nesting (class methods/fields, enum variants) by recursing into the relevant child nodes, mirroring the legacy `symbols_for_stmt` structure.

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::symbols`
Expected: PASS after adapting to the real CST kinds.

- [ ] **Step 4: Switch the server handler to the new provider**

In `src/lsp/server.rs` `document_symbol`, replace the `analysis::document_symbols(text)` call with the model-based one:

```rust
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let symbols = crate::lsp::providers::symbols::document_symbols(model);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
```

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`
Expected: PASS.

```bash
git add src/lsp/providers src/lsp/mod.rs src/lsp/server.rs
git commit -m "feat(lsp): port documentSymbol onto the CST model (providers/symbols.rs)"
```

---

## Task 9: Port `hover` onto the model

**Files:**
- Create: `src/lsp/providers/hover.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod hover;`), `src/lsp/server.rs` (`hover` handler)
- Test: inline in `src/lsp/providers/hover.rs`

- [ ] **Step 1: Write the failing test**

Create `src/lsp/providers/hover.rs`. The hover combines: keyword/builtin docs (reuse the existing static tables — move `keyword_doc`/`builtin_doc` from `analysis.rs` into this module or a shared `providers/docs.rs`), declaration kind from the resolver, and the inferred type from `check::infer::hover_type_at`.

```rust
//! `textDocument/hover` over the model: declaration/keyword/builtin docs plus the
//! SP10 inferred/declared type.

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

/// Hover at byte `offset`. Returns inferred type (if any) plus a doc line.
pub fn hover(model: &SemanticModel, offset: usize) -> Option<Hover> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ty) = crate::check::infer::hover_type_at(&model.text, offset) {
        parts.push(format!("```ascript\n{ty}\n```"));
    }
    if let Some(doc) = super::docs::doc_at(model, offset) {
        parts.push(doc);
    }
    if parts.is_empty() {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n---\n\n"),
        }),
        range: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn hover_on_typed_let_shows_type() {
        let src = "let x: number = 1\nprint(x)\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let off = src.rfind('x').unwrap(); // the use in print(x)
        let h = hover(&model, off).expect("hover");
        let HoverContents::Markup(m) = h.contents else { panic!() };
        assert!(m.value.contains("number"), "got {}", m.value);
    }
}
```

- [ ] **Step 2: Create the shared docs table**

Create `src/lsp/providers/docs.rs` and MOVE `keyword_doc`, `builtin_doc`, and a CST-based `doc_at(model, offset)` (find the token at offset; if it's a keyword/builtin/known decl, return its doc) out of the legacy `analysis.rs`. Register `pub mod docs;` in `providers/mod.rs`. Use `model.tokens` (the cached `LexToken`s) to find the token under the cursor rather than re-lexing.

- [ ] **Step 3: Run to verify it passes**

Run: `cargo test --lib lsp::providers::hover`
Expected: PASS.

- [ ] **Step 4: Switch the server handler**

In `src/lsp/server.rs` `hover`, compute the byte offset from the position via the model's `line_index` and call the new provider:

```rust
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let offset = crate::lsp::providers::docs::byte_offset_at(model, position);
        Ok(crate::lsp::providers::hover::hover(model, offset))
```

Add `byte_offset_at(model, Position) -> usize` to `providers/docs.rs` (char offset via `model.line_index.offset(position)` then char→byte via `convert`).

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers src/lsp/server.rs
git commit -m "feat(lsp): port hover onto the model (declaration/keyword docs + inferred type)"
```

---

## Task 10: Port `definition` onto the resolver/workspace

**Files:**
- Create: `src/lsp/providers/navigation.rs`
- Modify: `src/lsp/providers/mod.rs`, `src/lsp/server.rs` (`goto_definition`)
- Test: inline in `src/lsp/providers/navigation.rs`

- [ ] **Step 1: Write the failing test**

Create `src/lsp/providers/navigation.rs`. For single-file, resolve via `model.resolved.uses` (the `NameRef` whose `text_range()` contains the offset → its `Resolution`); for cross-file, defer to the workspace index (already implemented in `workspace.rs::definition_at`). Phase 0 only needs single-file parity plus keeping the existing cross-file path.

```rust
//! `textDocument/definition` over the resolver: a name use → its binding's decl.

use crate::lsp::model::SemanticModel;
use crate::syntax::resolve::types::Resolution;
use tower_lsp::lsp_types::Range;

/// In-file definition: the decl range of the binding the name at `offset` resolves
/// to (Local/Upvalue). Returns `None` for globals/unresolved (the server then asks
/// the workspace index, which handles cross-file + module globals).
pub fn definition_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    // Find the NameRef token range containing the offset.
    let nameref = model
        .tree
        .descendants()
        .find(|n| n.kind() == crate::syntax::kind::SyntaxKind::NameRef
            && {
                let r = n.text_range();
                let (s, e): (usize, usize) = (r.start().into(), r.end().into());
                offset >= s && offset < e
            })?;
    let res = model.resolved.uses.get(&nameref.text_range())?;
    let decl_range = match res {
        Resolution::Local(_) | Resolution::Upvalue(_) => {
            // Find the binding whose slot/frame matches by decl_range lookup:
            // bindings carry decl_range; match the nearest binding for this name.
            let name = crate::syntax::resolve::ident_text(&nameref)?;
            model.resolved.bindings.iter().find(|b| b.name == name).map(|b| b.decl_range)?
        }
        _ => return None,
    };
    Some(crate::lsp::convert::byte_span_to_range(
        &model.text, &model.line_index, crate::check::ByteSpan::from(decl_range),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn resolves_local_let() {
        let src = "fn f() {\n  let y = 1\n  return y\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let use_off = src.rfind('y').unwrap();
        let r = definition_in_file(&model, use_off).expect("def");
        assert_eq!(r.start.line, 1); // the `let y` line
    }
}
```

The binding lookup above is simplified; if the resolver exposes a use→binding mapping more precisely (check `ResolveResult` for a slot→binding index), use it for correctness with shadowing. Otherwise keep the cross-file workspace path as the primary and use this only when the workspace index returns nothing.

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test --lib lsp::providers::navigation`
Expected: PASS.

- [ ] **Step 3: Switch the server handler**

In `goto_definition`, try the workspace index first (existing behavior), then fall back to `definition_in_file`. Keep returning `GotoDefinitionResponse::Scalar(Location{uri, range})`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers src/lsp/server.rs
git commit -m "feat(lsp): port in-file definition onto the resolver (providers/navigation.rs)"
```

---

## Task 11: Port `completion` onto the model (behavior-preserving)

Phase 0 keeps completion's *current behavior* (baseline + import-path + namespace member) but reads from the model instead of re-lexing/parsing. The full rewrite is Phase 1.

**Files:**
- Create: `src/lsp/providers/completion.rs` (move the existing logic from `analysis.rs`)
- Modify: `src/lsp/providers/mod.rs`, `src/lsp/server.rs` (`completion`)
- Test: move the existing completion tests from `analysis.rs`

- [ ] **Step 1: Move the completion functions**

Move `completions`, `in_import_path_string`, `member_access_alias`, `namespace_import_module`, `is_ident_char`, `baseline_completions`, `item`, `KEYWORDS`, `BUILTINS`, `STD_MODULE_PATHS` from `analysis.rs` into `providers/completion.rs`. Change the public entry to `pub fn completions(model: &SemanticModel, offset: usize) -> Vec<CompletionItem>` taking `&model.text` internally. Keep the logic byte-identical.

- [ ] **Step 2: Move the tests**

Move the `completions_*` tests from `analysis.rs` into `providers/completion.rs`, adapting each to build a model first:

```rust
fn items(src: &str) -> Vec<CompletionItem> {
    let model = SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default());
    completions(&model, src.chars().count())
}
```

- [ ] **Step 3: Run to verify parity**

Run: `cargo test --lib lsp::providers::completion`
Expected: PASS — same assertions as before (`baseline has keywords and builtins`, `import path offers module paths`, `member access offers module exports`, `garbage returns baseline`).

- [ ] **Step 4: Switch the server handler**

In `completion`, fetch the model and call `providers::completion::completions(model, offset)`.

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers src/lsp/server.rs src/lsp/analysis.rs
git commit -m "feat(lsp): port completion onto the model (behavior-preserving; Phase 1 rewrites it)"
```

---

## Task 12: Refactor `workspace.rs` to accept cached text from the model

The workspace index re-parses on `file_module_arity`/`definition_at`/etc. Phase 0's minimal change: ensure the **open** document's analysis uses the cached model's text (already true via the server passing `&model.text`), and add a unit test pinning that cross-file navigation still works. (A deeper "borrow the cached model" refactor is deferred to Phase 1 to keep Phase 0 low-risk.)

**Files:**
- Modify: `src/lsp/workspace.rs` (no behavior change; add a doc note + a regression test)
- Test: inline in `src/lsp/workspace.rs`

- [ ] **Step 1: Add a cross-file regression test**

Append to `src/lsp/workspace.rs` tests (follow the existing hermetic temp-dir fixture pattern already in that file):

```rust
#[test]
fn definition_resolves_across_files_after_phase0() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib.as");
    let main = dir.path().join("main.as");
    fs::write(&lib, "export fn helper() { return 1 }\n").unwrap();
    fs::write(&main, "import { helper } from \"./lib\"\nlet x = helper()\n").unwrap();
    let idx = WorkspaceIndex::build_from_files(&[
        (lib.clone(), fs::read_to_string(&lib).unwrap()),
        (main.clone(), fs::read_to_string(&main).unwrap()),
    ]);
    let text = fs::read_to_string(&main).unwrap();
    let off = text.rfind("helper").unwrap();
    let def = idx.definition_at(&crate::lsp::workspace::canon(&main), off);
    assert!(def.is_some(), "cross-file helper() should resolve to lib.as");
}
```

Adapt the fixture/calls to the real `WorkspaceIndex` constructor + `definition_at` signature (confirmed present in `workspace.rs`).

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib lsp::workspace`
Expected: PASS.

```bash
git add src/lsp/workspace.rs
git commit -m "test(lsp): pin cross-file definition through Phase 0 refactor"
```

---

## Task 13: Delete the legacy front-end from the LSP

**Files:**
- Modify: `src/lsp/analysis.rs` (remove ported functions + the legacy imports)
- Test: a guard test asserting no legacy imports remain

- [ ] **Step 1: Remove the now-dead code**

In `src/lsp/analysis.rs`, delete every function moved in Tasks 8–11 (`document_symbols`, `hover`, `definition`, `completions` + their private helpers + the legacy `symbols_for_stmt`, `decl_doc`, `enclosing_fn_local`, etc.). Keep ONLY what the server still imports by that path: `byte_diagnostic_to_lsp` and `diagnostics` if still referenced — but `diagnostics` is now replaced by `model.lsp_diagnostics()`, so remove it too and drop the `analysis::diagnostics` import from `server.rs`. Remove these imports from the top of `analysis.rs`:

```rust
use crate::ast::{Expr, ExprKind, MatchArm, Param, Pattern, Stmt};
use crate::lexer;
use crate::parser;
use crate::token::Tok;
```

If `analysis.rs` ends up holding only `byte_diagnostic_to_lsp`, that is fine; otherwise delete the file and move that helper into `convert.rs`, updating `server.rs`.

- [ ] **Step 2: Write the guard test**

Add to `src/lsp/convert.rs` tests (or a new `src/lsp/mod.rs` test):

```rust
#[test]
fn lsp_does_not_import_legacy_frontend() {
    // The LSP must not reference the legacy interpreter front-end. This guards the
    // unification: any re-introduction of `crate::ast/lexer/parser/token` in the
    // lsp tree fails CI.
    for file in ["analysis.rs", "server.rs", "model.rs", "convert.rs"] {
        let path = format!("{}/src/lsp/{}", env!("CARGO_MANIFEST_DIR"), file);
        if let Ok(src) = std::fs::read_to_string(&path) {
            for banned in ["crate::ast", "crate::lexer", "crate::parser::", "crate::token"] {
                assert!(!src.contains(banned), "{file} still imports legacy {banned}");
            }
        }
    }
}
```

- [ ] **Step 3: Run to verify it fails, then fix until green**

Run: `cargo test --lib lsp`
Expected: the guard test FAILS until every legacy reference is removed. Remove them, then it PASSES.

- [ ] **Step 4: Update the module doc comment**

In `src/lsp/mod.rs` and `src/lsp/server.rs`, update the stale "reuses the interpreter's lexer/parser" comments to reflect that the LSP now runs on the CST front-end + cached `SemanticModel` exclusively.

- [ ] **Step 5: Full gate**

Run:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all green/clean.

- [ ] **Step 6: Commit**

```bash
git add src/lsp
git commit -m "refactor(lsp): delete the legacy front-end from the LSP — fully on the CST model"
```

---

## Phase 0 Done — Gate

- [ ] All 8 current capabilities behave identically (protocol smoke test green).
- [ ] `rg 'crate::(ast|lexer|parser::|token)' src/lsp/` returns nothing (guard test enforces).
- [ ] Diagnostics honor `ascript.toml [lint]` (config-aware path live).
- [ ] Incremental sync passes the full-reparse differential.
- [ ] `cargo test`, `cargo test --no-default-features`, and both clippy configs are green.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase1-editing-essentials.md` (formatting, completion rewrite, code actions).
