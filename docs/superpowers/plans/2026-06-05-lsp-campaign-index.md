# First-Class LSP — Campaign Index & Reconciliation Notes

Spec: `docs/superpowers/specs/2026-06-05-lsp-first-class-design.md`. This index ties the
eight phase plans together for parallel execution and records the cross-plan API truths
the plan authors confirmed against source (so implementers don't rediscover them).

## Phases & plans

| Phase | Plan file | Tasks | Depends on |
|---|---|---|---|
| 0 — Unification foundation | `2026-06-05-lsp-phase0-unification-foundation.md` | 13 | — |
| 1 — Editing essentials | `2026-06-05-lsp-phase1-editing-essentials.md` | 14 | Phase 0 |
| 2 — Semantic visualization | `2026-06-05-lsp-phase2-semantic-visualization.md` | 12 | Phase 0 |
| 3 — Navigation & structure depth | `2026-06-05-lsp-phase3-navigation-structure-depth.md` | 11 | Phase 0 |
| 4 — Advanced editing & workspace | `2026-06-05-lsp-phase4-advanced-editing-workspace.md` | 14 | Phase 0 |
| 5 — Grammar promotion | `2026-06-05-lsp-phase5-grammar-promotion.md` | 12 | — (independent) |
| 6 — Editor extensions | `2026-06-05-lsp-phase6-editor-extensions.md` | 15 | Phases 0–4 (server), Phase 5 (grammar) |
| 7 — Performance & polish | `2026-06-05-lsp-phase7-performance-polish.md` | 10 | Phases 0–4 |

## Dependency reality for "start all at once"

- **Phase 0 is a hard prerequisite** for Phases 1–4 and 7 — it creates `SemanticModel`,
  `DocumentStore`, `convert`, and `providers/`. Land it first (or on a base branch the
  others branch from).
- **Phase 5 (grammar) is fully independent** — start it in parallel with Phase 0
  immediately; it touches only the tree-sitter grammar + queries.
- **Phases 1–4 are mutually independent** once Phase 0 exists (different providers,
  different `server.rs` handlers) — ideal for parallel worktrees. The one shared file is
  `server.rs` `server_capabilities()` and the `execute_command_provider`/`workspace`
  capability structs: **EXTEND, never overwrite** (Phases 1, 4 both add commands; Phase 4
  multi-root extends `workspace`). Merge-order these capability edits carefully.
- **Phase 6 needs the server feature-complete (0–4) and the grammar (5).** Extension
  scaffolding (manifests, language-config, TextMate) can start early; the
  grammar-consuming bits (Zed/Neovim queries) gate on Phase 5.
- **Phase 7 optimizes Phase 0's cache** and tests the union of all providers — it lands
  last.

## Confirmed cross-plan API truths (ground these, do not re-litigate)

- **CST decl kinds:** `SyntaxKind::LetStmt` (NOT `LetDecl`), `FnDecl`, `ClassDecl`,
  `EnumDecl`, `EnumVariant`, `ImportStmt`, `NameRef`, `CallExpr`, `ArgList`, `MemberExpr`,
  `ArrayExpr`, `ObjectExpr`, `MatchExpr`, `Literal`, `Str`, `Number`, `Ident`,
  `LineComment` (`src/syntax/kind.rs`). The Phase 0 plan was corrected `LetDecl`→`LetStmt`.
- **`LexToken { kind, text }` has NO position field** (`src/syntax/lexer.rs:9`). Phase 2
  Task 1 derives byte spans by cumulative `text.len()` over the lossless token stream;
  any phase needing token offsets reuses that helper, not a non-existent span on the token.
- **`ByteSpan::from(node.text_range())`** via `usize::from(range.start())`
  (`src/check/diagnostic.rs`). Conversion to LSP `Range` is ONLY through
  `crate::lsp::convert::byte_span_to_range(src, &line_index, span)`.
- **Two distinct `TextEdit` types:** `crate::check::diagnostic::TextEdit { range:
  ByteSpan, replacement: String }` (analysis edits) vs `tower_lsp::lsp_types::TextEdit {
  range: Range, new_text: String }` (protocol). Convert at the boundary; never conflate.
- **`std_module_exports` returns `Vec<(String, Value)>`** — touches `Value`. The LSP stays
  `Value`-free: map to names only (`.map(|(name, _)| …)`), never store a `Value`. `std/math`
  is un-gated, so use it in `--no-default-features`-safe tests.
- **Type info:** `crate::check::infer::hover_type_at(src, byte) -> Option<String>`; hover
  ranges are recorded on `NameRef` *uses*, so a decl's NAME offset may have no type — use
  the init-expr offset for inlay/decl hints (Phase 2 note).
- **`infer::table::Table`** (`build`, `class_id`/`class`, `enum_id`/`enum_info`,
  `is_subclass`, `ClassInfo.fields/methods`, `EnumInfo.variants`) backs typeDefinition /
  implementation / typeHierarchy (Phases 1, 3).
- **Workspace index** (`src/lsp/workspace.rs`): `def_at`, `definition_at`, `references_at`,
  `workspace_symbols`, `rename_edits`, `import_specifier`/`resolve_specifier`,
  `build_from_files`, `canon` — backs cross-file nav, call/type hierarchy, file-rename
  import rewrite. Cross-file tests use its hermetic temp-dir fixture pattern.
- **Resolver:** `ResolveResult { uses: HashMap<TextRange, Resolution>, bindings:
  Vec<Binding>, frames }`; `Resolution::{Local(u32), Upvalue(u32), Global(String),
  Unresolved}`; `Binding { name, kind: BindingKind, decl_range, mutable, is_global, … }`.
  `decl_range` spans the WHOLE decl (starts at the `let`/`fn` keyword), not just the name.

## Version-sensitive items to confirm at implementation time

- **tower-lsp `lsp_types` names** for the newer providers (`Color`/`ColorPresentation`,
  LSP-3.17 pull-diagnostic report types, file-operation + work-done-progress structs,
  `WorkspaceSymbolOptions`, the `*ProviderCapability` enums). Confirm against the pinned
  `tower-lsp` version in `Cargo.lock` before writing the capability structs.
- **Zed:** `zed_extension_api` crate version + WASM target (`wasm32-wasip2` vs
  `wasm32-wasip1`) — Phase 6 targets `0.6`/`wasip2` with a documented fallback.
- **VS Code:** `vscode-languageclient ^9`, engine `^1.84`.
- **Minimum server version** pinned at `0.6.0` across all three editor integrations — bump
  at release.

## Front-end / front-matter invariants (all phases)

- The LSP stays static-only, `Send + Sync`, no interpreter (`Rc`/`RefCell`/`Value`).
- No `crate::{ast, lexer, parser, token}` anywhere under `src/lsp/` — Phase 0 Task 13's
  grep-guard test enforces it; every later phase uses `crate::syntax::*` exclusively.
- Gates per phase: `cargo test`, `cargo test --no-default-features`, `cargo clippy
  --all-targets`, `cargo clippy --no-default-features --all-targets` — all clean.
