# LSP capabilities

`ascript lsp` speaks LSP over stdio. Every method below is answered by the server;
each is powered by the cached per-document semantic model (CST + resolver + the SP10
advisory type inferencer) — the server never runs your code. The list mirrors
`server_capabilities()` in `src/lsp/server.rs`, the single source of truth.

## Lifecycle & sync

| Method | Notes |
|---|---|
| `initialize` / `initialized` / `shutdown` / `exit` | Standard lifecycle. `initialize` reports `serverInfo.version`. |
| `textDocument/didOpen` / `didChange` / `didClose` | **Incremental** sync (`TextDocumentSyncKind::INCREMENTAL`); rapid edits are debounced/coalesced into one rebuild. |

## Diagnostics

| Method | Notes |
|---|---|
| `textDocument/publishDiagnostics` | Pushed on open/change. Config-aware — honors the nearest `ascript.toml [lint]`, identical to `ascript check`. |
| `textDocument/diagnostic` (pull) | On-demand single-file diagnostics (same result as the push path). |
| `workspace/diagnostic` (pull) | Project-wide; advertises `interFileDependencies` + `workspaceDiagnostics`. |

## Navigation

| Method | Notes |
|---|---|
| `textDocument/definition` | Cross-file, follows import edges. |
| `textDocument/declaration` | Declaration ≈ definition (AScript has no separate forward-declaration concept). |
| `textDocument/typeDefinition` | Jumps to a value's class/enum declaration (in-file). |
| `textDocument/implementation` | Subclasses of a class / variants of an enum (in-file). |
| `textDocument/references` | Cross-file. |
| `textDocument/documentHighlight` | Read/write occurrences of the symbol under the cursor. |

## Symbols & structure

| Method | Notes |
|---|---|
| `textDocument/documentSymbol` | Nested (class → fields/methods, enum → variants); fields before methods. |
| `workspace/symbol` (+ `workspaceSymbol/resolve`) | Searches every `.as` file in the workspace; lazy resolve. |
| `textDocument/foldingRange` | Blocks, declarations, literals, `match`, and `//region` markers. |
| `textDocument/selectionRange` | Smart expand via CST ancestry. |
| `textDocument/documentLink` | Clickable import specifiers (relative → target file). |

## Hover, help & completion

| Method | Notes |
|---|---|
| `textDocument/hover` | Inferred/declared type (SP10) plus docs. |
| `textDocument/signatureHelp` | Active parameter while typing a call. Triggers on `(` and `,`. |
| `textDocument/completion` (+ `completionItem/resolve`) | Scope bindings, members, fields/methods, enum variants, module exports, keywords, control-flow snippets, and auto-import items. Triggers on `.`, `"`, `'`. |

## Editing power-tools

| Method | Notes |
|---|---|
| `textDocument/formatting` / `rangeFormatting` | Canonical layout, same output as `ascript fmt`. |
| `textDocument/codeAction` (+ `codeAction/resolve`) | Quick-fixes, `source.organizeImports`, `source.fixAll`. |
| `workspace/executeCommand` | `ascript.fixAll` (server-applied); `ascript.run` / `ascript.runTest` (acknowledged — the editor extension binds these to a terminal task, preserving the static-only invariant). |
| `textDocument/codeLens` (+ `codeLens/resolve`) | Run `test(...)`/`main`, reference counts; resolved lazily. |
| `textDocument/semanticTokens/full` / `range` | Types, params, properties, enums, with the provider legend. Large files: range-only. |
| `textDocument/inlayHint` (+ `inlayHint/resolve`) | Inferred `let`/param types and param-name hints. Skipped on large files. |
| `textDocument/rename` / `prepareRename` | Cross-file rename; refuses on collision or a parse error in a touched file. |
| `textDocument/linkedEditingRange` | Live rename of a local identifier's same-file occurrences (globals refused). |

## Hierarchy

| Method | Notes |
|---|---|
| `textDocument/prepareCallHierarchy` (+ `callHierarchy/incomingCalls` / `outgoingCalls`) | Over the workspace-index call graph. |
| `textDocument/prepareTypeHierarchy` (+ `typeHierarchy/supertypes` / `subtypes`) | Classes and enums. Advertised via the `experimental` capability (lsp-types 0.94 has no standard field; tower-lsp routes the method regardless). |

## Workspace & files

| Method | Notes |
|---|---|
| Multi-root workspace folders | `workspace.workspaceFolders` supported, with change notifications. |
| `workspace/didChangeWorkspaceFolders` | Re-roots the index. |
| `workspace/didChangeWatchedFiles` | Dynamically registered for `**/*.as` + `**/ascript.toml`. |
| `workspace/didChangeConfiguration` | Re-resolves lint config and republishes. |
| `workspace/willRenameFiles` / `didRenameFiles` | Rewrites imports on move, restricted to `file://**/*.as`. |

## Color

| Method | Notes |
|---|---|
| `textDocument/documentColor` / `colorPresentation` | `color.rgb` / `color.bgRgb`, `[r, g, b]` tui arrays, and gated hex/functional color strings. |

## Progress

| Method | Notes |
|---|---|
| `window/workDoneProgress/create` + `$/progress` | Initial workspace indexing reports begin → report N/total → end. |
| `window/workDoneProgress/cancel` | Aborts in-progress indexing (the `ascript-index` token). |

## Performance & limits

- **Debounce/coalesce:** bursts of keystrokes fold into one rebuild.
- **Supersession:** a completion/hover computed against now-stale text is dropped (the
  client re-requests against the fresh document).
- **Large-file bounds:** above ~256 KiB, `semanticTokens/full` goes range-only and inlay
  hints are skipped; above ~2 MiB, `semanticTokens/full`/inlay/folding/color providers go
  quiet. `semanticTokens/range` is **always** served — it is the bounded fallback that keeps
  the visible viewport colored at any file size. Diagnostics and navigation always run.
  Every degradation is logged via `window/logMessage`.
- **Indexing progress:** initial workspace indexing reports cancellable work-done
  progress.

> The AScript front-end is a full-reparse CST design; the LSP gets its responsiveness
> from debouncing and size bounds rather than incremental green-node reuse.
