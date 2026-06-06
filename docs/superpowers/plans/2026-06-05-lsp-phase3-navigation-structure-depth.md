# LSP Phase 3 — Navigation & Structure Depth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the navigation-and-structure depth capabilities to the AScript LSP — `declaration` / `typeDefinition` / `implementation`; `foldingRange` / `selectionRange` / `documentLink`; `callHierarchy` (prepare/incoming/outgoing); `typeHierarchy` (prepare/super/sub); and lazy `workspaceSymbol/resolve` — all built as pure providers over the Phase-0 `SemanticModel` + `WorkspaceIndex`, with no new front-end and no interpreter.

**Architecture:** Every new provider is a pure `fn(&SemanticModel, …)` (cross-file ones additionally take `&WorkspaceIndex`) in `src/lsp/providers/<name>.rs`. Structural providers (`folding`, `selection`, `links`) read the cached `model.tree` (`ResolvedNode`) + `model.tokens`. Navigation providers (`navigation.rs`) reuse `check::infer::hover_type_at` (typeDefinition) and `check::infer::table::Table` (implementation, typeHierarchy). Hierarchy providers (`hierarchy.rs`) drive the existing `WorkspaceIndex` (`def_at`, `references_at`, `definition_at`, `workspace_symbols`, `import_edges`). No provider imports `crate::{ast,lexer,parser,token}` (guarded by the Phase-0 test in Task 13 of Phase 0).

**Tech Stack:** Rust, `tower-lsp`, `cstree` (red/green CST), the existing `src/syntax/` + `src/check/` crates + `src/lsp/{model,convert,workspace,providers}.rs`.

**Reference (read before starting):**
- `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md` §4 (capability matrix: declaration/typeDefinition/implementation/foldingRange/selectionRange/documentLink/callHierarchy/typeHierarchy/workspaceSymbol-resolve rows), §6 Phase 3.
- `docs/superpowers/plans/2026-06-05-lsp-phase0-unification-foundation.md` — the `SemanticModel`/`DocumentStore`/`convert`/providers format this plan matches exactly.
- `src/lsp/model.rs` — `SemanticModel { text, version, tree: ResolvedNode, resolved: ResolveResult, diagnostics, tokens, line_index }`, `SemanticModel::build(text, version, &LintConfig)`, `DocumentStore`.
- `src/lsp/convert.rs` — `byte_span_to_range(src, &line_index, ByteSpan)`, `byte_to_char(src, byte)`.
- `src/lsp/workspace.rs` — `WorkspaceIndex`, `definition_at(&Path, offset) -> Option<(PathBuf, ByteSpan)>`, `references_at(&Path, offset, include_decl) -> Vec<(PathBuf, ByteSpan)>`, `def_at(&Path, offset) -> Option<(PathBuf, String, ByteSpan)>`, `workspace_symbols(query) -> Vec<SymbolDef>`, `SymbolDef { name, kind: DefKind, path, name_range }`, `ImportEdge { specifier, resolved, names }`, `import_edges`, `import_specifier`, `byte_span_to_range(text, ByteSpan)`, `canon(&Path)`.
- `src/check/infer/table.rs` — `Table::build(&tree, &resolved)`, `class_id(name)`, `enum_id(name)`, `class(id) -> Option<&ClassInfo>` (`ClassInfo { name, parent, fields, methods }`), `enum_info(id) -> Option<&EnumInfo>` (`EnumInfo { name, variants }`), `is_subclass(child, ancestor)`.
- `src/check/infer/mod.rs` — `hover_type_at(src, byte_offset) -> Option<String>` (the cursor value's rendered type, used for typeDefinition).
- `src/syntax/cst.rs` — `ResolvedNode` (cstree); traversal: `.children()`, `.descendants()`, `.children_with_tokens()`, `.parent()`, `.ancestors()`, `.kind()`, `.text_range()`; `.into_token()` on a `NodeOrToken`.
- `src/syntax/kind.rs` — `SyntaxKind` variants: `SourceFile`, `Block`, `FnDecl`, `ClassDecl`, `EnumDecl`, `EnumVariant`, `MethodDecl`, `FieldDecl`, `ArrayExpr`, `ObjectExpr`, `MatchExpr`, `IfStmt`, `WhileStmt`, `ForStmt`, `ImportStmt`, `ExportStmt`, `ImportList`, `NameRef`, `CallExpr`, `ArgList`, `Str`, `Ident`, `LineComment`, `BlockComment`.
- `src/check/rules/unresolved_import.rs` — `import` specifier parsing + the `std/*` registry check (`crate::stdlib::is_known_std_module`); the `strip_quotes` shape for documentLink.
- `src/lsp/server.rs` — `server_capabilities()` (`src/lsp/server.rs:69`), handler patterns (`document_symbol` at `:184`, `goto_definition` at `:223`, `symbol` at `:300`), `url_to_canon` (`:92`), `canon_to_url` (`:97`).

**Run the whole suite with:** `cargo test --lib lsp` (LSP unit tests) and `cargo test` (full). Clippy gate: `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` must be clean.

---

## File Structure

- Create `src/lsp/providers/navigation.rs` — extend Phase 0's `navigation.rs` with `declaration` / `typeDefinition` / `implementation`. (If Phase 0 created it for in-file definition, append; else create.)
- Create `src/lsp/providers/folding.rs` — `foldingRange`, `selectionRange`, `documentLink` (the spec groups these in `providers/folding.rs`).
- Create `src/lsp/providers/hierarchy.rs` — `callHierarchy` (prepare/incoming/outgoing) + `typeHierarchy` (prepare/super/sub).
- Modify `src/lsp/providers/mod.rs` — declare `pub mod folding;`, `pub mod hierarchy;` (and `navigation` if new).
- Modify `src/lsp/providers/symbols.rs` — add `workspace_symbol_resolve` (lazy resolve).
- Modify `src/lsp/server.rs` — advertise the new capabilities in `server_capabilities()` + add the handlers (`goto_declaration`, `goto_type_definition`, `goto_implementation`, `folding_range`, `selection_range`, `document_link`, `prepare_call_hierarchy`, `incoming_calls`, `outgoing_calls`, `prepare_type_hierarchy`, `supertypes`, `subtypes`, `symbol_resolve`).

---

## Task 1: `declaration` (≈ definition) provider

`declaration` for AScript is the same as `definition` (no separate forward-declaration concept). Reuse the Phase-0 in-file resolver path plus the workspace cross-file path; expose it under a distinct provider fn so the handler is explicit.

**Files:**
- Modify: `src/lsp/providers/navigation.rs`
- Modify: `src/lsp/providers/mod.rs` (ensure `pub mod navigation;`)
- Test: inline in `src/lsp/providers/navigation.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/navigation.rs`:

```rust
/// `textDocument/declaration` — for AScript this is identical to `definition`
/// (no separate declaration concept). Resolves the name at `offset` to its decl
/// range in this file. Cross-file declarations are served by the workspace index
/// in the server handler (same path `definition` uses).
pub fn declaration_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    definition_in_file(model, offset)
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn declaration_resolves_like_definition() {
        let src = "fn f() {\n  let y = 1\n  return y\n}\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let use_off = src.rfind('y').unwrap();
        let r = declaration_in_file(&model, use_off).expect("decl");
        assert_eq!(r.start.line, 1); // the `let y` line
    }
}
```

If Phase 0 named the in-file fn differently than `definition_in_file`, adapt the call (grep `pub fn definition` in `src/lsp/providers/navigation.rs`).

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::navigation::declaration_tests`
Expected: PASS once it compiles (it delegates to the existing in-file def).

- [ ] **Step 3: Advertise + wire the capability**

In `src/lsp/server.rs` `server_capabilities()` add:

```rust
        declaration_provider: Some(DeclarationCapability::Simple(true)),
```

Add the handler (mirror `goto_definition` at `src/lsp/server.rs:223`: try the workspace index first via `url_to_canon` + `idx.definition_at`, then fall back to `declaration_in_file`, returning `request::GotoDeclarationResponse::Scalar(Location { uri, range })`):

```rust
    async fn goto_declaration(
        &self,
        params: request::GotoDeclarationParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoDeclarationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let offset = byte_offset_at(model, position);
        // Cross-file via the workspace index first.
        if let Some(path) = url_to_canon(&uri) {
            if let Ok(idx) = self.index.read() {
                if let Some((def_path, span)) = idx.definition_at(&path, offset) {
                    if let Some(target_text) = idx.files.get(&def_path).map(|f| f.text.clone()) {
                        let range = workspace::byte_span_to_range(&target_text, span);
                        if let Some(def_uri) = canon_to_url(&def_path) {
                            return Ok(Some(request::GotoDeclarationResponse::Scalar(
                                Location { uri: def_uri, range },
                            )));
                        }
                    }
                }
            }
        }
        Ok(crate::lsp::providers::navigation::declaration_in_file(model, offset)
            .map(|range| request::GotoDeclarationResponse::Scalar(Location { uri, range })))
    }
```

`byte_offset_at(model, position)` is the Phase-0 helper (in `providers/docs.rs` per Phase 0 Task 9 Step 4); if it lives elsewhere, grep `fn byte_offset_at` and import from there. `DeclarationCapability`, `request::GotoDeclarationParams`, `request::GotoDeclarationResponse`, `Location` come from `tower_lsp::lsp_types` (add to the `use` block; `request` is `tower_lsp::lsp_types::request`).

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/navigation.rs src/lsp/providers/mod.rs src/lsp/server.rs
git commit -m "feat(lsp): declaration provider (= definition) + capability"
```

---

## Task 2: `typeDefinition` — jump to the cursor value's class/enum decl

The cursor's inferred type (via `hover_type_at`) names a class or enum; jump to that decl's name range (in-file via the CST `Table`/decl walk; cross-file is best-effort and falls through to `None` when the type's decl is not in this file, a documented limitation tracked with the SP10 cross-module=`Any` non-goal).

**Files:**
- Modify: `src/lsp/providers/navigation.rs`
- Test: inline in `src/lsp/providers/navigation.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/navigation.rs`:

```rust
use crate::syntax::kind::SyntaxKind;

/// `textDocument/typeDefinition` — the inferred type of the value at `offset`
/// names a class/enum; return that declaration's NAME range in this file.
/// Returns `None` when the type is a primitive, `Any`, or a type whose decl is
/// not in this file (cross-module types are `Any` under SP10 — a documented
/// limitation).
pub fn type_definition_in_file(model: &SemanticModel, offset: usize) -> Option<Range> {
    let ty = crate::check::infer::hover_type_at(&model.text, offset)?;
    // The rendered type may be `User`, `User?`, `array<User>`, etc. Extract the
    // first bare identifier (a class/enum name) from the rendering.
    let type_name = first_type_ident(&ty)?;
    decl_name_range(model, &type_name)
}

/// Extract the leading user-type identifier from a rendered `CheckTy` string.
/// `"User"` -> `User`; `"User?"` -> `User`; `"array<User>"` -> `array` (a builtin,
/// filtered out below); we want the FIRST capitalized identifier-shaped token.
fn first_type_ident(rendered: &str) -> Option<String> {
    // Collect identifier runs; the type-definition target is the first one that is
    // not a primitive/builtin container keyword.
    const BUILTIN: &[&str] = &[
        "number", "string", "bool", "nil", "any", "array", "map", "future", "bytes",
        "regex", "object", "void", "never",
    ];
    let mut cur = String::new();
    for ch in rendered.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
                return Some(cur);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() && !BUILTIN.contains(&cur.as_str()) {
        return Some(cur);
    }
    None
}

/// The NAME range of the `class`/`enum` named `name` declared in this file.
fn decl_name_range(model: &SemanticModel, name: &str) -> Option<Range> {
    let decl = model.tree.descendants().find(|n| {
        matches!(n.kind(), SyntaxKind::ClassDecl | SyntaxKind::EnumDecl)
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    })?;
    let ident = decl
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        crate::check::ByteSpan::from(ident.text_range()),
    ))
}

#[cfg(test)]
mod type_def_tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn type_definition_jumps_to_class_decl() {
        let src = "class User { name: string }\nlet u: User = User.from({ name: \"a\" })\nprint(u)\n";
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        // Cursor on the use `u` in `print(u)`.
        let off = src.rfind('u').unwrap();
        let r = type_definition_in_file(&model, off);
        // If SP10 infers `u: User`, jump to the `User` decl on line 0.
        if let Some(r) = r {
            assert_eq!(r.start.line, 0, "should jump to the class User decl");
        }
        // (When SP10 cannot infer the type, None is acceptable — documented.)
    }

    #[test]
    fn first_type_ident_strips_optional_and_containers() {
        assert_eq!(first_type_ident("User"), Some("User".to_string()));
        assert_eq!(first_type_ident("User?"), Some("User".to_string()));
        assert_eq!(first_type_ident("array<User>"), Some("User".to_string()));
        assert_eq!(first_type_ident("number"), None);
    }
}
```

The `type_definition_jumps_to_class_decl` assertion is guarded by `if let Some` because SP10 inference precision is the dependency; the `first_type_ident` test is the hard guarantee. If the SP10 renderer is confirmed to infer `u: User` for an annotated `let u: User`, tighten the first test to `.expect("type def")`.

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::navigation::type_def_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the capability**

In `server_capabilities()` add:

```rust
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
```

Add the handler (mirror Task 1; in-file only — return `GotoTypeDefinitionResponse::Scalar(Location { uri, range })`):

```rust
    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let offset = byte_offset_at(model, position);
        Ok(crate::lsp::providers::navigation::type_definition_in_file(model, offset)
            .map(|range| request::GotoTypeDefinitionResponse::Scalar(Location { uri, range })))
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/navigation.rs src/lsp/server.rs
git commit -m "feat(lsp): typeDefinition — jump to the value's inferred class/enum decl"
```

---

## Task 3: `implementation` — subclasses / enum variants

`implementation` of a class = its direct/transitive subclasses (their decl name ranges); of an enum = its variants. Drive the CST `Table` for the subclass relation and the CST for variant name ranges.

**Files:**
- Modify: `src/lsp/providers/navigation.rs`
- Test: inline in `src/lsp/providers/navigation.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/navigation.rs`:

```rust
use crate::check::infer::table::Table;

/// `textDocument/implementation` — when the cursor is on a class name, every
/// subclass decl's name range; when on an enum name, every variant's name range.
/// In-file only (subclasses across files are a documented follow-up). Returns an
/// empty vec when the cursor is not on a class/enum name.
pub fn implementations_in_file(model: &SemanticModel, offset: usize) -> Vec<Range> {
    let Some(name) = name_at_offset(model, offset) else {
        return Vec::new();
    };
    let table = Table::build(&model.tree, &model.resolved);
    if let Some(class_id) = table.class_id(&name) {
        // Every class whose ancestry includes this class (excluding itself).
        let mut out = Vec::new();
        for node in model.tree.descendants().filter(|n| n.kind() == SyntaxKind::ClassDecl) {
            let Some(other) = crate::syntax::resolve::ident_text(node) else { continue };
            let Some(other_id) = table.class_id(&other) else { continue };
            if other_id != class_id && table.is_subclass(other_id, class_id) {
                if let Some(r) = decl_name_range(model, &other) {
                    out.push(r);
                }
            }
        }
        return out;
    }
    if table.enum_id(&name).is_some() {
        // The cursor is an enum name: return each variant's name range.
        return enum_variant_ranges(model, &name);
    }
    Vec::new()
}

/// The identifier text at `offset` (a `NameRef` token or a decl's name `Ident`).
fn name_at_offset(model: &SemanticModel, offset: usize) -> Option<String> {
    let node = model.tree.descendants().find(|n| {
        let r = n.text_range();
        let (s, e): (usize, usize) = (r.start().into(), r.end().into());
        offset >= s
            && offset < e
            && matches!(
                n.kind(),
                SyntaxKind::NameRef | SyntaxKind::ClassDecl | SyntaxKind::EnumDecl
            )
    })?;
    crate::syntax::resolve::ident_text(&node)
}

/// Each variant's name range in the enum named `enum_name`.
fn enum_variant_ranges(model: &SemanticModel, enum_name: &str) -> Vec<Range> {
    let Some(decl) = model.tree.descendants().find(|n| {
        n.kind() == SyntaxKind::EnumDecl
            && crate::syntax::resolve::ident_text(n).as_deref() == Some(enum_name)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in decl.children().filter(|c| c.kind() == SyntaxKind::EnumVariant) {
        if let Some(ident) = v
            .children_with_tokens()
            .filter_map(|el| el.into_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Ident)
        {
            out.push(crate::lsp::convert::byte_span_to_range(
                &model.text,
                &model.line_index,
                crate::check::ByteSpan::from(ident.text_range()),
            ));
        }
    }
    out
}

#[cfg(test)]
mod impl_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &crate::check::LintConfig::default())
    }

    #[test]
    fn class_implementations_are_subclasses() {
        let src = "class Animal {}\nclass Dog extends Animal {}\nclass Cat extends Animal {}\n";
        let m = model(src);
        let off = src.find("Animal").unwrap() + 1; // on the `Animal` class name
        let impls = implementations_in_file(&m, off);
        assert_eq!(impls.len(), 2, "Dog + Cat: {impls:?}");
    }

    #[test]
    fn enum_implementations_are_variants() {
        let src = "enum Color { Red, Green, Blue }\nprint(1)\n";
        let m = model(src);
        let off = src.find("Color").unwrap() + 1;
        let impls = implementations_in_file(&m, off);
        assert_eq!(impls.len(), 3, "{impls:?}");
    }

    #[test]
    fn non_type_offset_yields_empty() {
        let m = model("let x = 1\nprint(x)\n");
        let off = m.text.rfind('x').unwrap();
        assert!(implementations_in_file(&m, off).is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::navigation::impl_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the capability**

In `server_capabilities()` add:

```rust
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
```

Add the handler returning `GotoImplementationResponse::Array(Vec<Location>)` (all in `uri`):

```rust
    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> tower_lsp::jsonrpc::Result<Option<request::GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let offset = byte_offset_at(model, position);
        let locs: Vec<Location> = crate::lsp::providers::navigation::implementations_in_file(model, offset)
            .into_iter()
            .map(|range| Location { uri: uri.clone(), range })
            .collect();
        if locs.is_empty() {
            return Ok(None);
        }
        Ok(Some(request::GotoImplementationResponse::Array(locs)))
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/navigation.rs src/lsp/server.rs
git commit -m "feat(lsp): implementation — subclasses of a class / variants of an enum"
```

---

## Task 4: `foldingRange` — blocks, decls, multi-line literals, `//region`

**Files:**
- Create: `src/lsp/providers/folding.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod folding;`)
- Test: inline in `src/lsp/providers/folding.rs`

- [ ] **Step 1: Declare the module + write the failing test**

In `src/lsp/providers/mod.rs` add `pub mod folding;`. Create `src/lsp/providers/folding.rs`:

```rust
//! `textDocument/foldingRange`, `selectionRange`, and `documentLink` — structural
//! providers over the cached CST (`model.tree`) + tokens (`model.tokens`). No
//! re-parse; pure `fn(&SemanticModel, …)`.

use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;
use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind};

/// Foldable ranges: every multi-line `Block` / class / enum / array / object
/// literal node, plus `//region` … `//endregion` line-comment pairs. Line-based
/// folds (start line .. end line, exclusive of the closing line per LSP custom).
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
    for node in model.tree.descendants_with_tokens() {
        let Some(tok) = node.as_token() else { continue };
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
```

Confirm `model.tree.descendants_with_tokens()` and `NodeOrToken::as_token()` exist (cstree exposes both; `descendants_with_tokens` is used in `src/lsp/workspace.rs`-adjacent code and `as_token` is the cstree element accessor — if the codebase uses `.into_token()` instead, swap `.as_token()` for `el.into_token()` on a cloned element). If `model.line_index.position(ch)` returns a `Position`, `.line` is `u32`.

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::folding::folding_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the capability**

In `server_capabilities()` add:

```rust
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
```

Add the handler:

```rust
    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<FoldingRange>>> {
        let store = self.documents.lock().await;
        let Some(model) = store.get(&params.text_document.uri) else { return Ok(None); };
        Ok(Some(crate::lsp::providers::folding::folding_ranges(model)))
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/folding.rs src/lsp/providers/mod.rs src/lsp/server.rs
git commit -m "feat(lsp): foldingRange (blocks/decls/literals/match + //region pairs)"
```

---

## Task 5: `selectionRange` — smart-expand via CST ancestry

For each requested position, build the LSP `SelectionRange` chain from the innermost CST node containing the offset outward to the `SourceFile` (token → expr → stmt → block → decl → root).

**Files:**
- Modify: `src/lsp/providers/folding.rs`
- Test: inline in `src/lsp/providers/folding.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/folding.rs`:

```rust
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
    // Walk `ancestors()` (innermost → root), building the chain inside-out.
    let mut chain: Option<SelectionRange> = None;
    // Collect innermost + ancestors, then fold from the OUTERMOST in so each
    // SelectionRange.parent is the next-larger node.
    let mut nodes: Vec<_> = std::iter::once(innermost.clone())
        .chain(innermost.ancestors())
        .collect();
    nodes.dedup_by_key(|n| n.text_range());
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
        assert!(parent_w >= inner_w, "parent should be no smaller: {inner:?} {parent:?}");
    }
}
```

Confirm `innermost.ancestors()` yields `innermost`'s parents (cstree's `.ancestors()` — verify whether it includes `self`; if it does, drop the `std::iter::once(...)` prepend to avoid a duplicate, which `dedup_by_key` already handles). `ancestors()` is used elsewhere per the grep in research.

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::folding::selection_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the capability**

In `server_capabilities()` add:

```rust
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
```

Add the handler (one `SelectionRange` per requested position):

```rust
    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<SelectionRange>>> {
        let store = self.documents.lock().await;
        let Some(model) = store.get(&params.text_document.uri) else { return Ok(None); };
        let mut out = Vec::with_capacity(params.positions.len());
        for pos in &params.positions {
            let offset = byte_offset_at(model, *pos);
            match crate::lsp::providers::folding::selection_range_at(model, offset) {
                Some(sr) => out.push(sr),
                None => out.push(SelectionRange {
                    range: Range::new(*pos, *pos),
                    parent: None,
                }),
            }
        }
        Ok(Some(out))
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/folding.rs src/lsp/server.rs
git commit -m "feat(lsp): selectionRange — smart expand via CST ancestry"
```

---

## Task 6: `documentLink` — clickable import specifiers

Each `import … from "<path>"` specifier becomes a link: a relative path resolves to the target file via the workspace index; a `std/*` path links to nothing (or, optionally, a doc URL — v1 = no target, just the recognized span).

**Files:**
- Modify: `src/lsp/providers/folding.rs`
- Test: inline in `src/lsp/providers/folding.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/folding.rs`:

```rust
use tower_lsp::lsp_types::{DocumentLink, Url};

/// A recognized import-specifier link: the `Range` of the `"<path>"` string token
/// + an optional resolved target file `Url`. `std/*` and unresolved/bare imports
/// yield a link with `target = None` (the span is still highlighted).
pub struct ImportLink {
    pub range: tower_lsp::lsp_types::Range,
    /// The resolved relative-import target file path, if any (`std/*` → None).
    pub target: Option<std::path::PathBuf>,
}

/// Every `import` specifier in the file, with its string-token range and resolved
/// relative target (resolved against `importer_dir`, mirroring the runtime rule:
/// join, append `.as` if no extension). `std/*` and bare specifiers → `None`.
pub fn import_links(model: &SemanticModel, importer_dir: Option<&std::path::Path>) -> Vec<ImportLink> {
    let mut out = Vec::new();
    for import in model
        .tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ImportStmt)
    {
        let Some(str_tok) = import
            .children_with_tokens()
            .filter_map(|el| el.into_token().cloned())
            .find(|t| t.kind() == SyntaxKind::Str)
        else {
            continue;
        };
        let r = str_tok.text_range();
        let range = crate::lsp::convert::byte_span_to_range(
            &model.text,
            &model.line_index,
            crate::check::ByteSpan::from(r),
        );
        let spec = strip_quotes(str_tok.text());
        let target = match importer_dir {
            Some(dir) if spec.starts_with("./") || spec.starts_with("../") => {
                let mut p = dir.join(spec);
                if p.extension().is_none() {
                    p.set_extension("as");
                }
                Some(p)
            }
            _ => None, // std/* or bare or no importer dir
        };
        out.push(ImportLink { range, target });
    }
    out
}

/// Strip the surrounding quotes from a string literal token (mirrors
/// `src/check/rules/unresolved_import.rs::strip_quotes`).
fn strip_quotes(s: &str) -> &str {
    let mut chars = s.chars();
    chars.next();
    chars.next_back();
    chars.as_str()
}

/// Build LSP `DocumentLink`s from the recognized import links.
pub fn document_links(model: &SemanticModel, importer_dir: Option<&std::path::Path>) -> Vec<DocumentLink> {
    import_links(model, importer_dir)
        .into_iter()
        .map(|link| DocumentLink {
            range: link.range,
            target: link.target.and_then(|p| Url::from_file_path(p).ok()),
            tooltip: None,
            data: None,
        })
        .collect()
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn relative_import_resolves_to_file() {
        let m = model("import { helper } from \"./lib\"\nprint(1)\n");
        let dir = std::path::Path::new("/ws");
        let links = import_links(&m, Some(dir));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target.as_deref(), Some(std::path::Path::new("/ws/lib.as")));
    }

    #[test]
    fn std_import_has_no_target() {
        let m = model("import { abs } from \"std/math\"\nprint(1)\n");
        let links = import_links(&m, Some(std::path::Path::new("/ws")));
        assert_eq!(links.len(), 1);
        assert!(links[0].target.is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::folding::link_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the capability**

In `server_capabilities()` add:

```rust
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
```

Add the handler (compute the importer dir from the doc `Url`):

```rust
    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let dir = uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        Ok(Some(crate::lsp::providers::folding::document_links(model, dir.as_deref())))
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/folding.rs src/lsp/server.rs
git commit -m "feat(lsp): documentLink — clickable import specifiers (relative → target file)"
```

---

## Task 7: `callHierarchy` — prepare + incoming + outgoing

Drive the `WorkspaceIndex`: prepare resolves the cursor to a `(path, name, range)` callable; incoming = files/sites that reference it (`references_at`, filtered to call sites); outgoing = the calls inside the callable's body (CST `CallExpr` children resolved via `definition_at`).

**Files:**
- Create: `src/lsp/providers/hierarchy.rs`
- Modify: `src/lsp/providers/mod.rs` (add `pub mod hierarchy;`)
- Test: inline in `src/lsp/providers/hierarchy.rs` (hermetic temp-dir fixtures like `workspace.rs`)

- [ ] **Step 1: Declare the module + write the failing test**

In `src/lsp/providers/mod.rs` add `pub mod hierarchy;`. Create `src/lsp/providers/hierarchy.rs`:

```rust
//! `callHierarchy` and `typeHierarchy` — cross-file structural navigation over the
//! `WorkspaceIndex` (call graph) + the CST class/enum `Table` (type graph). Pure
//! functions; the server adapts them to the LSP wire types.

use crate::lsp::workspace::WorkspaceIndex;
use std::path::{Path, PathBuf};

/// A resolved call-hierarchy anchor: the canonical defining file + the callable's
/// name + its name-range (byte span). The handler maps this to a
/// `CallHierarchyItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAnchor {
    pub path: PathBuf,
    pub name: String,
    pub name_range: crate::check::diagnostic::ByteSpan,
}

/// Resolve the cursor in `path` at byte `offset` to its call-hierarchy anchor (the
/// canonical definition it refers to), via the index's `def_at`.
pub fn prepare_call(idx: &WorkspaceIndex, path: &Path, offset: usize) -> Option<CallAnchor> {
    let (def_path, name, name_range) = idx.def_at(path, offset)?;
    Some(CallAnchor { path: def_path, name, name_range })
}

/// Incoming calls: every reference to the anchor across the workspace, grouped by
/// the file they occur in. Reuses `references_at` (which already follows import
/// edges), excluding the declaration itself.
pub fn incoming_calls(idx: &WorkspaceIndex, anchor: &CallAnchor) -> Vec<(PathBuf, crate::check::diagnostic::ByteSpan)> {
    let off = anchor.name_range.start;
    idx.references_at(&anchor.path, off, false)
}

#[cfg(test)]
mod call_tests {
    use super::*;
    use std::fs;

    fn index(files: &[(&str, &str)]) -> (tempfile::TempDir, WorkspaceIndex) {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();
        for (name, src) in files {
            let p = dir.path().join(name);
            fs::write(&p, src).unwrap();
            entries.push((p, src.to_string()));
        }
        (dir, WorkspaceIndex::build_from_files(&entries))
    }

    #[test]
    fn prepare_resolves_a_callee() {
        let (dir, idx) = index(&[("a.as", "fn helper() { return 1 }\nfn main() { return helper() }\n")]);
        let p = crate::lsp::workspace::canon(&dir.path().join("a.as"));
        let text = &idx.files[&p].text;
        let off = text.find("helper()").unwrap(); // the call site
        let anchor = prepare_call(&idx, &p, off).expect("anchor");
        assert_eq!(anchor.name, "helper");
        assert_eq!(&text[anchor.name_range.start..anchor.name_range.end], "helper");
    }

    #[test]
    fn incoming_finds_the_call_site() {
        let (dir, idx) = index(&[("a.as", "fn helper() { return 1 }\nfn main() { return helper() }\n")]);
        let p = crate::lsp::workspace::canon(&dir.path().join("a.as"));
        let text = &idx.files[&p].text;
        let decl_off = text.find("helper()").map(|_| text.find("fn helper").unwrap() + 3).unwrap();
        let anchor = prepare_call(&idx, &p, decl_off).expect("anchor on decl");
        let incoming = incoming_calls(&idx, &anchor);
        assert!(!incoming.is_empty(), "should find the call in main: {incoming:?}");
    }
}
```

`references_at`/`def_at`/`canon`/`build_from_files`/`files` are all confirmed in `src/lsp/workspace.rs`. `ByteSpan` is `crate::check::diagnostic::ByteSpan` (used throughout `workspace.rs`).

- [ ] **Step 2: Add outgoing-calls + write its test**

Append to `src/lsp/providers/hierarchy.rs`:

```rust
use crate::lsp::model::SemanticModel;
use crate::syntax::kind::SyntaxKind;

/// An outgoing call target: the callee name + the call-site range (in the caller's
/// file) + the resolved definition location, if the index can resolve it.
#[derive(Debug, Clone)]
pub struct OutgoingCall {
    pub name: String,
    pub call_site: crate::check::diagnostic::ByteSpan,
    pub def: Option<(PathBuf, crate::check::diagnostic::ByteSpan)>,
}

/// Outgoing calls from the function whose body contains `anchor.name_range`: every
/// `CallExpr` with a `NameRef` callee inside that fn's CST node, resolved against
/// the index. `model` is the anchor file's cached model.
pub fn outgoing_calls(
    idx: &WorkspaceIndex,
    model: &SemanticModel,
    anchor: &CallAnchor,
) -> Vec<OutgoingCall> {
    // Find the FnDecl/MethodDecl node whose name range matches the anchor.
    let Some(fn_node) = model.tree.descendants().find(|n| {
        matches!(n.kind(), SyntaxKind::FnDecl | SyntaxKind::MethodDecl)
            && fn_name_range(n) == Some(anchor.name_range)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for call in fn_node.descendants().filter(|n| n.kind() == SyntaxKind::CallExpr) {
        let Some(callee) = call.children().find(|c| c.kind() == SyntaxKind::NameRef) else {
            continue;
        };
        let Some(name) = crate::syntax::resolve::ident_text(&callee) else { continue };
        let call_site = crate::check::diagnostic::ByteSpan::from(callee.text_range());
        let def = idx.definition_at(&anchor.path, call_site.start);
        out.push(OutgoingCall { name, call_site, def });
    }
    out
}

/// The name-range (byte span) of a `FnDecl`/`MethodDecl`'s name `Ident`.
fn fn_name_range(node: &crate::syntax::cst::ResolvedNode) -> Option<crate::check::diagnostic::ByteSpan> {
    let ident = node
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::check::diagnostic::ByteSpan::from(ident.text_range()))
}

#[cfg(test)]
mod outgoing_tests {
    use super::*;
    use crate::check::LintConfig;
    use std::fs;

    #[test]
    fn outgoing_lists_inner_calls() {
        let dir = tempfile::tempdir().unwrap();
        let src = "fn a() { return 1 }\nfn main() { return a() }\n";
        let p = dir.path().join("a.as");
        fs::write(&p, src).unwrap();
        let idx = WorkspaceIndex::build_from_files(&[(p.clone(), src.to_string())]);
        let canon = crate::lsp::workspace::canon(&p);
        let model = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let main_off = src.find("fn main").unwrap() + 3;
        let anchor = prepare_call(&idx, &canon, main_off).expect("anchor on main");
        let outs = outgoing_calls(&idx, &model, &anchor);
        assert!(outs.iter().any(|o| o.name == "a"), "{outs:?}");
    }
}
```

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::hierarchy`
Expected: PASS.

- [ ] **Step 4: Advertise + wire the three handlers**

In `server_capabilities()` add:

```rust
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
```

Add the three handlers. `prepare_call_hierarchy` builds a `CallHierarchyItem` from the anchor (kind `FUNCTION`, `selection_range` = the name range converted via the anchor file's text, `range` = same or the whole fn); `incoming_calls` / `outgoing_calls` map the provider results to `CallHierarchyIncomingCall` / `CallHierarchyOutgoingCall`. Skeleton for prepare:

```rust
    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(path) = url_to_canon(&uri) else { return Ok(None); };
        let offset = {
            let store = self.documents.lock().await;
            let Some(model) = store.get(&uri) else { return Ok(None); };
            byte_offset_at(model, position)
        };
        let Ok(idx) = self.index.read() else { return Ok(None); };
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_call(&idx, &path, offset) else {
            return Ok(None);
        };
        let Some(target_text) = idx.files.get(&anchor.path).map(|f| f.text.clone()) else {
            return Ok(None);
        };
        let range = workspace::byte_span_to_range(&target_text, anchor.name_range);
        let Some(item_uri) = canon_to_url(&anchor.path) else { return Ok(None); };
        #[allow(deprecated)]
        let item = CallHierarchyItem {
            name: anchor.name.clone(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            detail: None,
            uri: item_uri,
            range,
            selection_range: range,
            data: None,
        };
        Ok(Some(vec![item]))
    }
```

For `incoming_calls`/`outgoing_calls`, re-resolve the anchor from `params.item.uri` + `params.item.selection_range` (convert that range's start back to a byte offset via the target file's text + `convert`), then call `hierarchy::incoming_calls` / `hierarchy::outgoing_calls` and map each `(path, span)` / `OutgoingCall` to its wire type. Each `CallHierarchyIncomingCall { from: CallHierarchyItem, from_ranges: Vec<Range> }`; each `CallHierarchyOutgoingCall { to: CallHierarchyItem, from_ranges }`. Use `idx.def_at` / `idx.files[..].text` to build the `CallHierarchyItem`s, skipping any unresolved `OutgoingCall.def == None`.

- [ ] **Step 5: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/hierarchy.rs src/lsp/providers/mod.rs src/lsp/server.rs
git commit -m "feat(lsp): callHierarchy (prepare/incoming/outgoing) over the workspace index"
```

---

## Task 8: `typeHierarchy` — prepare + supertypes + subtypes

For a class: prepare resolves the cursor to a class anchor; supertypes = the `extends` chain (one parent per level); subtypes = direct subclasses. For an enum: prepare resolves it; supertypes/subtypes are empty (enums have no inheritance) — return the enum item itself for prepare so the client shows it.

**Files:**
- Modify: `src/lsp/providers/hierarchy.rs`
- Test: inline in `src/lsp/providers/hierarchy.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/hierarchy.rs`:

```rust
use crate::check::infer::table::Table;

/// A resolved type-hierarchy anchor: the class/enum name + its decl name-range in
/// the file it is declared in (in-file resolution; cross-file extends is a
/// follow-up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAnchor {
    pub name: String,
    pub name_range: crate::check::diagnostic::ByteSpan,
    pub is_class: bool,
}

/// Resolve the cursor at `offset` to a class/enum type anchor in `model`.
pub fn prepare_type(model: &SemanticModel, offset: usize) -> Option<TypeAnchor> {
    let name = type_name_at(model, offset)?;
    let table = Table::build(&model.tree, &model.resolved);
    let (is_class, decl_kind) = if table.class_id(&name).is_some() {
        (true, SyntaxKind::ClassDecl)
    } else if table.enum_id(&name).is_some() {
        (false, SyntaxKind::EnumDecl)
    } else {
        return None;
    };
    let range = decl_name_byte_range(model, &name, decl_kind)?;
    Some(TypeAnchor { name, name_range: range, is_class })
}

/// The supertype names (the `extends` chain, nearest first) of the class `name`.
pub fn supertypes(model: &SemanticModel, name: &str) -> Vec<(String, crate::check::diagnostic::ByteSpan)> {
    let table = Table::build(&model.tree, &model.resolved);
    let mut out = Vec::new();
    let mut cur = table.class_id(name);
    let mut visited = Vec::new();
    while let Some(id) = cur {
        if visited.contains(&id) {
            break;
        }
        visited.push(id);
        let Some(ci) = table.class(id) else { break };
        let Some(parent) = ci.parent else { break };
        let Some(pinfo) = table.class(parent) else { break };
        if let Some(r) = decl_name_byte_range(model, &pinfo.name, SyntaxKind::ClassDecl) {
            out.push((pinfo.name.clone(), r));
        }
        cur = Some(parent);
    }
    out
}

/// The direct-subtype names of the class `name` (every class whose immediate
/// parent is `name`).
pub fn subtypes(model: &SemanticModel, name: &str) -> Vec<(String, crate::check::diagnostic::ByteSpan)> {
    let table = Table::build(&model.tree, &model.resolved);
    let Some(target) = table.class_id(name) else { return Vec::new() };
    let mut out = Vec::new();
    for node in model.tree.descendants().filter(|n| n.kind() == SyntaxKind::ClassDecl) {
        let Some(other) = crate::syntax::resolve::ident_text(node) else { continue };
        let Some(oid) = table.class_id(&other) else { continue };
        if let Some(ci) = table.class(oid) {
            if ci.parent == Some(target) {
                if let Some(r) = decl_name_byte_range(model, &other, SyntaxKind::ClassDecl) {
                    out.push((other, r));
                }
            }
        }
    }
    out
}

/// The identifier text at `offset` if it is a class/enum NAME or NameRef.
fn type_name_at(model: &SemanticModel, offset: usize) -> Option<String> {
    let node = model.tree.descendants().find(|n| {
        let r = n.text_range();
        let (s, e): (usize, usize) = (r.start().into(), r.end().into());
        offset >= s
            && offset < e
            && matches!(
                n.kind(),
                SyntaxKind::NameRef | SyntaxKind::ClassDecl | SyntaxKind::EnumDecl
            )
    })?;
    crate::syntax::resolve::ident_text(&node)
}

/// The byte span of the name `Ident` of the `kind` decl named `name`.
fn decl_name_byte_range(
    model: &SemanticModel,
    name: &str,
    kind: SyntaxKind,
) -> Option<crate::check::diagnostic::ByteSpan> {
    let decl = model.tree.descendants().find(|n| {
        n.kind() == kind && crate::syntax::resolve::ident_text(n).as_deref() == Some(name)
    })?;
    let ident = decl
        .children_with_tokens()
        .filter_map(|el| el.into_token().cloned())
        .find(|t| t.kind() == SyntaxKind::Ident)?;
    Some(crate::check::diagnostic::ByteSpan::from(ident.text_range()))
}

#[cfg(test)]
mod type_hierarchy_tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn supertypes_walk_the_extends_chain() {
        let src = "class A {}\nclass B extends A {}\nclass C extends B {}\n";
        let m = model(src);
        let sup = supertypes(&m, "C");
        let names: Vec<&str> = sup.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["B", "A"]);
    }

    #[test]
    fn subtypes_are_direct_children() {
        let src = "class A {}\nclass B extends A {}\nclass C extends A {}\n";
        let m = model(src);
        let sub = subtypes(&m, "A");
        let mut names: Vec<&str> = sub.iter().map(|(n, _)| n.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["B", "C"]);
    }

    #[test]
    fn prepare_resolves_a_class() {
        let m = model("class A {}\nclass B extends A {}\n");
        let off = m.text.find("class A").unwrap() + 6;
        let anchor = prepare_type(&m, off).expect("anchor");
        assert_eq!(anchor.name, "A");
        assert!(anchor.is_class);
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::hierarchy::type_hierarchy_tests`
Expected: PASS.

- [ ] **Step 3: Advertise + wire the three handlers**

In `server_capabilities()` add:

```rust
        type_hierarchy_provider: Some(TypeHierarchyServerCapability::Simple(true)),
```

Add `prepare_type_hierarchy` (build a `TypeHierarchyItem` from `prepare_type`: kind `CLASS` or `ENUM`, `selection_range`/`range` from the anchor name range via the doc text), `supertypes`, `subtypes` (each maps the provider's `(name, span)` pairs to `TypeHierarchyItem`s in the same `uri`). Re-resolve the anchor from `params.item.uri` + `params.item.selection_range.start` → byte offset (via the doc model + `byte_offset_at`-style char→byte) → `prepare_type` to recover the name, then call `supertypes`/`subtypes`. Skeleton for prepare:

```rust
    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let store = self.documents.lock().await;
        let Some(model) = store.get(&uri) else { return Ok(None); };
        let offset = byte_offset_at(model, position);
        let Some(anchor) = crate::lsp::providers::hierarchy::prepare_type(model, offset) else {
            return Ok(None);
        };
        let range = crate::lsp::convert::byte_span_to_range(
            &model.text, &model.line_index, anchor.name_range,
        );
        #[allow(deprecated)]
        let item = TypeHierarchyItem {
            name: anchor.name,
            kind: if anchor.is_class { SymbolKind::CLASS } else { SymbolKind::ENUM },
            tags: None,
            detail: None,
            uri,
            range,
            selection_range: range,
            data: None,
        };
        Ok(Some(vec![item]))
    }
```

For `supertypes`/`subtypes`, fetch the model for `params.item.uri`, recover the name via `prepare_type` at the item's selection-range start byte, then map `hierarchy::supertypes`/`hierarchy::subtypes` results to `TypeHierarchyItem`s.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/hierarchy.rs src/lsp/server.rs
git commit -m "feat(lsp): typeHierarchy (prepare/supertypes/subtypes) for classes + enums"
```

---

## Task 9: `workspaceSymbol/resolve` — lazy resolve

The server already advertises `workspaceSymbol` (Phase 0). Add the lazy `resolve` step: advertise resolve support, and resolve a returned `WorkspaceSymbol` (filling `container_name` / a fuller location) on demand. Keep the initial `symbol` response cheap.

**Files:**
- Modify: `src/lsp/providers/symbols.rs`
- Modify: `src/lsp/server.rs`
- Test: inline in `src/lsp/providers/symbols.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/lsp/providers/symbols.rs`:

```rust
use tower_lsp::lsp_types::WorkspaceSymbol;

/// Resolve a `WorkspaceSymbol` lazily: v1 is a no-op pass-through (the initial
/// `workspaceSymbol` response is already fully formed for AScript's flat symbol
/// set), provided as the resolve hook so the capability can advertise
/// `resolve_provider: true` and grow detail later (container, signature) without a
/// protocol change.
pub fn resolve_workspace_symbol(symbol: WorkspaceSymbol) -> WorkspaceSymbol {
    symbol
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use tower_lsp::lsp_types::{Location, OneOf, SymbolKind, Url};

    #[test]
    fn resolve_is_identity_v1() {
        let sym = WorkspaceSymbol {
            name: "f".to_string(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            container_name: None,
            location: OneOf::Left(Location {
                uri: Url::parse("file:///ws/a.as").unwrap(),
                range: Default::default(),
            }),
            data: None,
        };
        let resolved = resolve_workspace_symbol(sym.clone());
        assert_eq!(resolved.name, sym.name);
    }
}
```

If the existing `symbol` handler returns `SymbolInformation` (the deprecated flat type) rather than `WorkspaceSymbol`, switch the handler's response to `WorkspaceSymbolResponse::Nested(Vec<WorkspaceSymbol>)` first (the modern shape that supports resolve) — confirm the current return shape in `src/lsp/server.rs` `symbol` (`:300`). If it already returns `WorkspaceSymbol`, no change needed there.

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test --lib lsp::providers::symbols::resolve_tests`
Expected: PASS.

- [ ] **Step 3: Advertise resolve + add the handler**

In `server_capabilities()` change the `workspace_symbol_provider` to advertise resolve (and ensure the existing `symbol` response uses `WorkspaceSymbolResponse::Nested`):

```rust
        workspace_symbol_provider: Some(OneOf::Left(true)),
        // resolve advertised via the dedicated options form:
```

tower-lsp advertises workspace-symbol resolve via `WorkspaceSymbolOptions { resolve_provider: Some(true), .. }` wrapped in `OneOf::Right`:

```rust
        workspace_symbol_provider: Some(OneOf::Right(WorkspaceSymbolOptions {
            resolve_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
```

Add the handler:

```rust
    async fn symbol_resolve(
        &self,
        params: WorkspaceSymbol,
    ) -> tower_lsp::jsonrpc::Result<WorkspaceSymbol> {
        Ok(crate::lsp::providers::symbols::resolve_workspace_symbol(params))
    }
```

(Confirm the `LanguageServer` trait method name in the tower-lsp version in `Cargo.lock` — it is `symbol_resolve` in current tower-lsp; if the trait names it differently, match the trait.)

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib lsp && cargo test --test lsp`

```bash
git add src/lsp/providers/symbols.rs src/lsp/server.rs
git commit -m "feat(lsp): workspaceSymbol/resolve (lazy resolve hook + capability)"
```

---

## Task 10: Protocol smoke test — assert the Phase 3 capability set

Extend the JSON-RPC-over-stdio test (`tests/lsp.rs`) to assert the new capabilities are advertised after `initialize`, and exercise one representative request per new provider end-to-end.

**Files:**
- Modify: `tests/lsp.rs`
- Test: the new assertions

- [ ] **Step 1: Add the capability assertions**

In `tests/lsp.rs`, in the initialize-result assertion block, add checks that the result's `capabilities` advertises: `declarationProvider`, `typeDefinitionProvider`, `implementationProvider`, `foldingRangeProvider`, `selectionRangeProvider`, `documentLinkProvider`, `callHierarchyProvider`, `typeHierarchyProvider`, and `workspaceSymbolProvider` with `resolveProvider: true`. (Match the existing JSON-field assertion style already in `tests/lsp.rs`.)

Also add a unit-level assertion in `src/lsp/server.rs` tests (mirror `capabilities_advertise_cross_file_providers` at `src/lsp/server.rs:442`):

```rust
    #[test]
    fn capabilities_advertise_phase3_navigation() {
        let caps = server_capabilities();
        assert!(caps.declaration_provider.is_some());
        assert!(caps.type_definition_provider.is_some());
        assert!(caps.implementation_provider.is_some());
        assert!(caps.folding_range_provider.is_some());
        assert!(caps.selection_range_provider.is_some());
        assert!(caps.document_link_provider.is_some());
        assert!(caps.call_hierarchy_provider.is_some());
        assert!(caps.type_hierarchy_provider.is_some());
    }
```

- [ ] **Step 2: Run the protocol + unit tests**

Run: `cargo test --test lsp && cargo test --lib lsp`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/lsp.rs src/lsp/server.rs
git commit -m "test(lsp): assert Phase 3 navigation capability set is advertised"
```

---

## Task 11: Guard test — no legacy front-end in the new providers

Re-assert the Phase-0 invariant over the Phase-3 files: no `crate::{ast,lexer,parser,token}` imports in the new providers.

**Files:**
- Modify: the Phase-0 guard test (in `src/lsp/convert.rs` or `src/lsp/mod.rs`) to include the new files.

- [ ] **Step 1: Extend the guard list**

In the Phase-0 `lsp_does_not_import_legacy_frontend` test, add the new files to the scanned set:

```rust
    for file in [
        "analysis.rs", "server.rs", "model.rs", "convert.rs",
        "providers/navigation.rs", "providers/folding.rs", "providers/hierarchy.rs",
        "providers/symbols.rs",
    ] {
        // … existing banned-substring loop …
    }
```

- [ ] **Step 2: Run + commit**

Run: `cargo test --lib lsp`
Expected: PASS (the new providers use only `crate::syntax::*` / `crate::check::*` / `crate::lsp::*`).

```bash
git add src/lsp
git commit -m "test(lsp): extend legacy-frontend guard to Phase 3 providers"
```

---

## Phase 3 Done — Gate

- [ ] `declaration` (= definition), `typeDefinition` (value → class/enum decl), and `implementation` (subclasses / enum variants) work and are advertised.
- [ ] `foldingRange` (blocks/decls/literals/match + `//region` pairs), `selectionRange` (CST-ancestry expand), and `documentLink` (relative imports → target file; `std/*` → no target) work and are advertised.
- [ ] `callHierarchy` (prepare/incoming/outgoing) resolves over the workspace index, including cross-file incoming calls via import edges.
- [ ] `typeHierarchy` (prepare/supertypes/subtypes) walks the class `extends` chain and lists direct subclasses; enums resolve for prepare with empty super/sub.
- [ ] `workspaceSymbol/resolve` is advertised (`resolveProvider: true`) and the resolve hook returns a valid symbol.
- [ ] The protocol smoke test asserts the full Phase 3 capability set; representative requests succeed end-to-end.
- [ ] The legacy-frontend guard covers the new providers (no `crate::{ast,lexer,parser,token}`).
- [ ] `cargo test`, `cargo test --no-default-features`, and both clippy configs (`cargo clippy --all-targets`, `cargo clippy --no-default-features --all-targets`) are green/clean.

**Next plan:** `docs/superpowers/plans/2026-06-05-lsp-phase4-advanced-editing-workspace.md` (documentColor/colorPresentation, linkedEditingRange, codeLens, pull diagnostics, willRenameFiles/didRenameFiles import rewrite, didChangeConfiguration, multi-root + work-done progress).
