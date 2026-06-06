# Editor setup

AScript ships a first-class language server (`ascript lsp`, stdio JSON-RPC) plus a
tree-sitter grammar and a canonical formatter (`ascript fmt`). This page gets each
supported editor talking to them. For the full list of methods the server answers,
see [LSP capabilities](lsp-capabilities).

All three editors discover the server on your `PATH` as `ascript`. Build it once:

```bash
cargo build --release      # → target/release/ascript
# put target/release on PATH, or copy `ascript` into ~/.local/bin
```

The integration sources live under [`editors/`](https://github.com/ascript-lang/ascript/tree/main/editors)
(`vscode/`, `zed/`, `nvim/`, plus a shared `README.md` describing the binary-discovery
policy and the minimum server version). Every extension follows the same discovery
order: the `ascript` binary on `PATH` first, then an explicit `ascript.server.path`
override.

## VS Code

Install the **AScript** extension (from `editors/vscode/`; package it with `vsce` or
load it unpacked). It launches `ascript lsp` over stdio and contributes the `.as`
language, a TextMate grammar for instant coloring, and semantic tokens served by the
language server.

Settings (`settings.json`):

```jsonc
{
  // Override the server binary if it is not on PATH:
  "ascript.server.path": "/absolute/path/to/ascript",
  // Trace LSP traffic when filing a bug:
  "ascript.trace.server": "verbose"
}
```

Command: **AScript: Restart Language Server**. Format-on-save uses
`textDocument/formatting`, which produces the same output as `ascript fmt`.

## Zed

Install the **AScript** extension from `editors/zed/`. It registers the tree-sitter
grammar (highlights / injections / locals / folds / indents / textobjects / brackets)
and launches the `ascript lsp` server.

**Preferred setup — let Zed find `ascript` itself.** Zed loads your login-shell environment,
so if `ascript` is on your shell `PATH` (e.g. add `~/.local/bin` in your `~/.zshrc`/`~/.bashrc`)
the extension launches it with the correct `lsp` argument automatically — no settings needed.

**If you must point Zed at an absolute binary** (e.g. it still can't find it), set BOTH `path`
**and** `arguments` — overriding `binary.path` makes Zed launch the binary directly and `arguments`
defaults to empty, so you must pass `["lsp"]` yourself:

```jsonc
{
  "lsp": {
    "ascript": { "binary": { "path": "/absolute/path/to/ascript", "arguments": ["lsp"] } }
  }
}
```

> [!WARN] Setting `binary.path` **without** `arguments: ["lsp"]` launches bare `ascript`,
> which prints CLI help instead of speaking LSP — Zed then reports *"Server reset the
> connection"* and you get syntax highlighting but no diagnostics/hover/navigation. The
> `arguments: ["lsp"]` field is required with `binary.path`.

> [!NOTE] Zed loads the tree-sitter grammar from the published
> [`ascript-lang/tree-sitter-ascript`](https://github.com/ascript-lang/tree-sitter-ascript)
> repo, pinned by `rev` in `editors/zed/extension.toml`. Every LSP capability (see the
> [capabilities page](lsp-capabilities)) — including the language server's
> semantic-token coloring — works regardless; the tree-sitter grammar adds the local
> syntax highlighting on top.

## Neovim

With [`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig) (0.10+):

```lua
vim.filetype.add({ extension = { as = "ascript" } })

local lspconfig = require("lspconfig")
local configs = require("lspconfig.configs")
if not configs.ascript then
  configs.ascript = {
    default_config = {
      cmd = { "ascript", "lsp" },
      filetypes = { "ascript" },
      root_dir = lspconfig.util.root_pattern("ascript.toml", ".git"),
      single_file_support = true,
    },
  }
end
lspconfig.ascript.setup({})
```

The `editors/nvim/` directory ships this as a small `ascript` Lua module so you can
`require("ascript").setup()` instead of inlining the config.

Tree-sitter highlighting via [`nvim-treesitter`](https://github.com/nvim-treesitter/nvim-treesitter)
registers the `ascript` parser plus the shared queries under `queries/ascript/`, and starts
highlighting automatically for `*.as` buffers. **Both** nvim-treesitter branches are supported — the
plugin detects whether you run the legacy **master** API or the **main** rewrite and registers
accordingly. Install the parser with `:TSInstall ascript`.

> [!NOTE] The tree-sitter parser is pulled from the published
> [`ascript-lang/tree-sitter-ascript`](https://github.com/ascript-lang/tree-sitter-ascript)
> repo (pinned by revision in the Neovim config). LSP features work regardless of the parser.

Formatting: use the LSP (`vim.lsp.buf.format()`), or a
[`conform.nvim`](https://github.com/stevearc/conform.nvim) recipe pointing at
`ascript fmt` as a fallback:

```lua
require("conform").setup({
  formatters = { ascript_fmt = { command = "ascript", args = { "fmt", "$FILENAME" }, stdin = false } },
  formatters_by_ft = { ascript = { "ascript_fmt" } },
})
```

## Troubleshooting: the editor can't find `ascript`

If you get **syntax coloring but no diagnostics, hover, or go-to-definition** (and maybe a
"could not find `ascript`" error), the editor's process can't locate the binary. The usual
cause is a **GUI-launched editor on macOS**: an app started from the Dock/Finder does *not*
inherit your shell's `PATH`, so a binary in `~/.local/bin` (or `~/.cargo/bin`) is invisible —
even though it works in your terminal.

- **VS Code** and **Neovim** now also search the common install dirs (`~/.local/bin`,
  `~/.cargo/bin`, `~/bin`, `/usr/local/bin`, `/opt/homebrew/bin`) automatically, so a
  standard install usually just works. To be explicit, set `ascript.server.path` (VS Code).
- **Zed** runs its extension in a WASM sandbox and can only resolve the binary via the
  worktree `PATH` or an explicit setting. Zed loads your login-shell env, so adding `~/.local/bin`
  to your shell `PATH` is usually enough. If you must set `lsp.ascript.binary.path`, you **must
  also** set `arguments: ["lsp"]` (overriding `binary.path` drops the extension's `lsp` argument —
  see the Zed section above); otherwise Zed runs bare `ascript` and reports "Server reset the
  connection".
- Alternatively, install `ascript` to a dir already on the GUI `PATH` (e.g. `/usr/local/bin`
  or `/opt/homebrew/bin`), or launch the editor from a terminal so it inherits your shell `PATH`.

## Performance notes

The server coalesces rapid keystrokes into a single rebuild and supersedes stale
completion/hover results, so editing stays responsive. Very large files degrade
gracefully — above ~256 KiB `semanticTokens/full` is served range-only and inlay hints are
skipped; above ~2 MiB `semanticTokens/full`/inlay/folding/color providers go quiet (but
`semanticTokens/range` is always served, keeping the visible viewport colored) — with a
note in the LSP log. Diagnostics and navigation always run. Initial workspace indexing reports
cancellable work-done progress. (The front-end is a full-reparse design; responsiveness
comes from debouncing, not incremental green-node reuse.)
