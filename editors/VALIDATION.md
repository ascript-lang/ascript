# AScript Editor Integrations — Manual UX Validation Matrix

This document is the **Phase-6 Task-15 deliverable**: a per-editor checklist that validates each
integration (VS Code, Zed, Neovim) against the full capability surface the `ascript lsp` server
advertises on `initialize`. Live editors cannot be driven headlessly in CI, so these steps are run
by hand on a real workspace before the Phase-6 gate is considered met.

Record results inline (tick the boxes) and note any divergence in the PR description. A divergence
is an **integration** bug (the client did not surface a capability the server advertises), not a
server bug — the server is fixed from Phases 0–5.

---

## 0. Prerequisites & fixture

Build/install the `ascript` binary and put it on `PATH` (or set the per-editor path override). Then
create a throwaway workspace that exercises every capability:

```
mkdir -p /tmp/ascript-uxtest && cd /tmp/ascript-uxtest
printf '[package]\nname = "uxtest"\n' > ascript.toml
```

`/tmp/ascript-uxtest/main.as`:

```as
import { abs } from "std/math"

// A class with a typed field and a method, to exercise hover/definition/rename.
class Point {
  x: number
  y: number

  fn dist(): number {
    return abs(self.x) + abs(self.y)
  }
}

fn make(n: number): Point {
  let p = Point()
  p.x = n
  p.y = n
  return p
}

fn main() {
  let p = make(3)
  print(p.dist())
}

test("dist is the manhattan norm", fn () {
  assert(make(2).dist() == 4)
})
```

`/tmp/ascript-uxtest/helper.as` (a second file, for cross-file navigation / rename / file-rename):

```as
import { make } from "./main"

fn twice(n: number): number {
  return make(n).dist() * 2
}
```

This fixture deliberately contains: an import (relative + std), a typed class/field/method, a call
across files, a `test(...)` registration and a `main` (for codeLens), a color-free baseline (add
`color.rgb(...)` if validating color), and a couple of intentional edits below to trigger
diagnostics.

---

## 1. Server capability surface (the source of truth)

Every row is a capability advertised by `server_capabilities()` in `src/lsp/server.rs`. The three
editor columns mark whether that editor's client surfaces it (✅ supported, ➖ not exposed by the
editor's LSP client, n/a). VS Code (`vscode-languageclient` v9) and Neovim (built-in `vim.lsp`)
expose the broadest surface; Zed exposes a curated subset.

| # | LSP capability (server field) | Request / feature | VS Code | Zed | Neovim |
|---|-------------------------------|-------------------|---------|-----|--------|
| 1 | `text_document_sync` = INCREMENTAL | live edits sync to server | ☐ | ☐ | ☐ |
| 2 | `diagnostic_provider` (+ push) | squiggles, pull `textDocument/diagnostic` | ☐ | ☐ | ☐ |
| 3 | `hover_provider` | hover shows inferred type | ☐ | ☐ | ☐ |
| 4 | `completion_provider` (`.` `"` `'`, resolve) | completion list + lazy resolve | ☐ | ☐ | ☐ |
| 5 | `signature_help_provider` (`(` `,`) | param hints while typing a call | ☐ | ➖ | ☐ |
| 6 | `definition_provider` | go-to-definition | ☐ | ☐ | ☐ |
| 7 | `declaration_provider` | go-to-declaration | ☐ | ➖ | ☐ |
| 8 | `type_definition_provider` | go-to-type-definition | ☐ | ➖ | ☐ |
| 9 | `implementation_provider` | go-to-implementation (subclasses/variants) | ☐ | ➖ | ☐ |
| 10 | `references_provider` | find all references (cross-file) | ☐ | ☐ | ☐ |
| 11 | `document_highlight_provider` | read/write occurrence highlight | ☐ | ☐ | ☐ |
| 12 | `document_symbol_provider` | outline / breadcrumbs | ☐ | ☐ | ☐ |
| 13 | `workspace_symbol_provider` (resolve) | workspace symbol search | ☐ | ☐ | ☐ |
| 14 | `rename_provider` (prepare) | rename symbol (cross-file) | ☐ | ☐ | ☐ |
| 15 | `linked_editing_range_provider` | live multi-occurrence local rename | ☐ | ➖ | ☐ |
| 16 | `document_formatting_provider` | Format Document | ☐ | ☐ | ☐ |
| 17 | `document_range_formatting_provider` | Format Selection | ☐ | ➖ | ☐ |
| 18 | `code_action_provider` (quickfix / fixAll / organizeImports, resolve) | lightbulb fixes | ☐ | ☐ | ☐ |
| 19 | `code_lens_provider` (resolve) | run-test / run-main + reference counts | ☐ | ➖ | ☐ |
| 20 | `execute_command_provider` (`ascript.fixAll`/`run`/`runTest`) | command invocation | ☐ | ➖ | ☐ |
| 21 | `folding_range_provider` | code folding (incl. `//region`) | ☐ | ☐ | ☐ |
| 22 | `selection_range_provider` | smart-expand selection | ☐ | ➖ | ☐ |
| 23 | `document_link_provider` | clickable import specifiers | ☐ | ➖ | ☐ |
| 24 | `call_hierarchy_provider` | incoming/outgoing calls | ☐ | ➖ | ☐ |
| 25 | type hierarchy (`experimental.typeHierarchyProvider`) | supertypes/subtypes | ☐ | ➖ | ☐ |
| 26 | `color_provider` | color swatches / presentations | ☐ | ➖ | ☐ |
| 27 | `inlay_hint_provider` (resolve) | inferred-type + param-name inlays | ☐ | ☐ | ☐ |
| 28 | `semantic_tokens_provider` (full + range) | semantic highlighting | ☐ | ☐ | ☐ |
| 29 | `workspace.workspace_folders` | multi-root workspaces | ☐ | ☐ | ☐ |
| 30 | `workspace.file_operations` (will/did rename, `*.as`) | import rewrite on file rename | ☐ | ➖ | ☐ |
| 31 | `server_info` (`name`/`version`) | min-version check (≥ 0.6.0) | ☐ | n/a | ☐ |

> The ➖ cells are not failures — they reflect what each editor's LSP client currently exposes
> (e.g. Zed routes a smaller method set). The hard gate is that **every row a given editor CAN
> surface, it DOES** — and that highlighting + the server attach (rows 2, 3, 6, 28) work in all
> three.

---

## 2. VS Code

**Install:**

```bash
cd editors/vscode && npm ci && npm run compile && npx vsce package --allow-missing-repository --no-dependencies -o ascript.vsix
code --install-extension ascript.vsix
```

Open `/tmp/ascript-uxtest/main.as`. Verify:

- [ ] **Activation & attach** — the extension activates on the `.as` file (or on
      `workspaceContains:**/ascript.toml`); the AScript Language Server starts (Output → "AScript
      Language Server" channel is populated).
- [ ] **TextMate highlighting** renders immediately (before the server warms): keywords, strings,
      template `${…}`, numbers, `class`/`fn`/`enum`/`let`/`const` names.
- [ ] **Semantic tokens** (row 28) layer on once the server attaches (e.g. `Point` colored as a
      type, `dist` as a method, params distinct).
- [ ] **Diagnostics** (row 2) — introduce an error (e.g. change `make(3)` to `make()`); a squiggle
      + Problems-panel entry appears and honors `ascript.toml [lint]`.
- [ ] **Hover** (row 3) shows the inferred type on `p`, `make`, `self.x`.
- [ ] **Completion** (row 4) after typing `p.` lists `x`, `y`, `dist`; `import { } from "` offers
      paths; lazy resolve fills detail/docs.
- [ ] **Signature help** (row 5) — typing `make(` shows the parameter list; `,` advances it.
- [ ] **Navigation** — Go to Definition / Declaration / Type Definition / Implementation (rows
      6–9) jump correctly; **Find All References** (row 10) lists uses across `main.as` + `helper.as`.
- [ ] **Document highlight** (row 11) — cursor on `make` highlights its occurrences.
- [ ] **Outline & breadcrumbs** (row 12) list `Point`, `dist`, `make`, `main`; **workspace symbol
      search** (row 13, `Ctrl/Cmd+T`) finds `twice` in `helper.as`.
- [ ] **Rename** (row 14) — rename `make`; the edit propagates to `helper.as`. Prepare-rename
      validates the symbol under the cursor.
- [ ] **Linked editing** (row 15) — editing a local binding name updates its same-file occurrences live.
- [ ] **Formatting** (rows 16–17) — **Format Document** and **Format Selection** reflow the file
      (via the server's formatter engine).
- [ ] **Code actions** (row 18) — the lightbulb offers quick fixes / Fix All / Organize Imports
      (e.g. remove an unused import).
- [ ] **CodeLens** (row 19) — "▶ Run | ▶ Run test" lenses appear above `main`/`test(...)`;
      reference counts above declarations. Clicking **▶ Run** invokes `ascript.run`, which opens
      the integrated "AScript" terminal and runs `ascript run <file>`; clicking **▶ Run test**
      invokes `ascript.runTest`, running `ascript test <file>` in that terminal (row 20). The CLI
      has no per-test name filter, so **▶ Run test** runs ALL of the file's `test(...)`
      registrations regardless of which lens was clicked. The same two commands are also available
      from the palette (**AScript: Run File** / **AScript: Run Tests**), acting on the active editor.
- [ ] **Folding** (row 21) — blocks/decls fold; a `//region`/`//endregion` pair folds.
- [ ] **Selection range** (row 22) — Expand Selection grows by CST ancestry.
- [ ] **Document links** (row 23) — `Ctrl/Cmd`-click the `"./main"` specifier opens the target file.
- [ ] **Call hierarchy** (row 24) / **Type hierarchy** (row 25) — Show Call Hierarchy on `make`;
      Show Type Hierarchy on `Point`.
- [ ] **Inlay hints** (row 27) — inferred-type + parameter-name hints render (toggle Editor › Inlay
      Hints if off).
- [ ] **Multi-root** (row 29) — add a second folder; both attach. **File rename** (row 30) — rename
      `main.as`; the `import "./main"` in `helper.as` is rewritten.
- [ ] **Commands** — **AScript: Restart Language Server** restarts cleanly; **AScript: Show
      Language Server Version** reports `ascript <version>`.
- [ ] **Version gate** (row 31) — against a server older than `0.6.0`, a warning toast appears.
- [ ] **Tracing** — set `ascript.trace.server: "verbose"`; the Output channel shows the JSON-RPC trace.
- [ ] **Server path override** — set `ascript.server.path` to an absolute path; the extension uses it.

---

## 3. Zed

**Install (dev extension):**

Command palette → **zed: install dev extension** → select `editors/zed`. (Build is automatic;
`cargo build --release --target wasm32-wasip2` must succeed first — see CI.)

Open `/tmp/ascript-uxtest/main.as`. Verify:

- [ ] **tree-sitter highlighting** (row 28-equivalent) renders (from the bundled `languages/ascript`
      queries; full set finalized once the Phase-5 grammar is published).
- [ ] **Server attach** — the AScript language server starts (Editor: open the LSP logs to confirm).
- [ ] **Diagnostics** (row 2) appear on an introduced error.
- [ ] **Hover** (row 3) shows the inferred type.
- [ ] **Go to Definition** (row 6) jumps; **Find All References** (row 10).
- [ ] **Document symbols / outline** (row 12) populate the outline panel.
- [ ] **Rename** (row 14) propagates cross-file.
- [ ] **Format** (row 16) on save / via the format action reflows the file.
- [ ] **Code actions** (row 18) offer quick fixes.
- [ ] **Inlay hints** (row 27) render if enabled in Zed settings.
- [ ] **Multi-root** (row 29) — both worktrees attach.
- [ ] **Binary-path override** — set in Zed settings and confirm it is honored:

      ```json
      { "lsp": { "ascript": { "binary": { "path": "/absolute/path/to/ascript" } } } }
      ```

> Zed's LSP client does not surface every advertised provider (the ➖ rows in §1). Those are
> expected gaps in the editor, not integration failures.

---

## 4. Neovim

**Setup** — with `nvim-lspconfig` (and optionally `nvim-treesitter`, `conform.nvim`) installed,
add to your config:

```lua
require("ascript").setup({ treesitter = true })
```

Open `/tmp/ascript-uxtest/main.as`. Verify:

- [ ] **Filetype** is `ascript` (`:set ft?`).
- [ ] **LSP attach** — `:LspInfo` lists the `ascript` client (`cmd = {"ascript","lsp"}`, root from
      `ascript.toml`/`.git`).
- [ ] **tree-sitter highlighting** (if `nvim-treesitter` + the grammar are installed) renders;
      otherwise built-in syntax/semantic-tokens still color the buffer.
- [ ] **Diagnostics** (row 2) — `:lua vim.diagnostic.open_float()` / signs appear on an error.
- [ ] **Hover** (row 3) — `:lua vim.lsp.buf.hover()` (`K`).
- [ ] **Completion** (row 4) — omnifunc / your completion plugin lists `p.x`/`p.y`/`p.dist`.
- [ ] **Signature help** (row 5) — `:lua vim.lsp.buf.signature_help()` while in `make(`.
- [ ] **Navigation** — `:lua vim.lsp.buf.definition()` / `declaration()` / `type_definition()` /
      `implementation()` (rows 6–9); `:lua vim.lsp.buf.references()` (row 10) lists cross-file uses.
- [ ] **Document highlight** (row 11) — `:lua vim.lsp.buf.document_highlight()`.
- [ ] **Symbols** — `:lua vim.lsp.buf.document_symbol()` (row 12) and
      `:lua vim.lsp.buf.workspace_symbol("twice")` (row 13).
- [ ] **Rename** (row 14) — `:lua vim.lsp.buf.rename()` on `make` updates `helper.as`.
- [ ] **Formatting** (rows 16–17) — `:lua vim.lsp.buf.format()` reflows. If `conform.nvim` is wired
      with `require("ascript.format").conform_formatter`, `:lua require("conform").format()` also
      formats — **via the tempfile strategy** (see the note below).
- [ ] **Code actions** (row 18) — `:lua vim.lsp.buf.code_action()` offers fixes.
- [ ] **CodeLens** (row 19/20) — `:lua vim.lsp.codelens.refresh()` then `:lua vim.lsp.codelens.run()`;
      the editor binds `ascript.run`/`ascript.runTest` to a terminal task.
- [ ] **Folding** (row 21), **Inlay hints** (row 27) — `:lua vim.lsp.inlay_hint.enable(true)`
      (Neovim ≥ 0.10).
- [ ] **Call/type hierarchy** (rows 24/25) — `:lua vim.lsp.buf.incoming_calls()` /
      `outgoing_calls()`; type hierarchy via the experimental method.
- [ ] **File rename** (row 30) — rename `main.as` via a file-ops-aware plugin (e.g. nvim-tree /
      oil) and confirm the import rewrite (server-driven via `willRenameFiles`).
- [ ] **Version gate** (row 31) — `M.min_server_version = "0.6.0"`; older servers should be flagged
      by the user's `on_attach` if it checks `serverInfo.version`.

> **Formatting correctness note (resolved in this phase).** The `ascript fmt` CLI formats files **in
> place** and has **no stdin mode** — `ascript fmt -` errors with `could not read -`. The
> `conform.nvim` recipe in `lua/ascript/format.lua` therefore uses conform's **tempfile** strategy
> (`stdin = false`, `args = { "fmt", "$FILENAME" }`): conform writes the buffer to a temp file, runs
> `ascript fmt` on it (rewriting it in place), and reads the result back. A `stdin = true` /
> `ascript fmt -` recipe would silently fail and must not be used. The recommended path remains LSP
> `textDocument/formatting`, which works unconditionally.

---

## 5. Results

Record the run here (date, editor versions, `ascript --version`) and link follow-up issues for any
gaps:

| Editor | Version | Result | Notes / follow-ups |
|--------|---------|--------|--------------------|
| VS Code |  | ☐ pass / ☐ gaps |  |
| Zed |  | ☐ pass / ☐ gaps |  |
| Neovim |  | ☐ pass / ☐ gaps |  |

### Known documented placeholders (finalize before publishing — Phase-5 dependency)

- **Zed / Neovim grammar repo URL + commit SHA** — RESOLVED. The standalone
  `ascript-lang/tree-sitter-ascript` grammar is published and `editors/zed/extension.toml`
  (`[grammars.ascript].commit`) and `editors/nvim/lua/ascript/treesitter.lua` (`GRAMMAR_REV`) are
  both pinned to the published commit (currently `7227fb7f`). Re-pin both whenever the grammar
  changes — `scripts/sync-grammar.sh` mirrors the subtree and prints the new SHA. The bundled
  `*.scm` query files are full copies of the complete query set (verified), not interim stubs.
- **VS Code icon** — the extension ships without an icon and uses VS Code's default file icon
  (`vsce package` succeeds with none present). To add Marketplace artwork later, drop a 128×128
  `editors/vscode/icons/ascript.png` and restore the two `package.json` references (see
  `editors/vscode/icons/README.md`). Not required to ship.
