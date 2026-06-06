# Editor setup

AScript ships a first-class language server (`ascript lsp`, stdio JSON-RPC) plus a
tree-sitter grammar and a canonical formatter (`ascript fmt`). This page gets each
supported editor talking to them. For the full list of methods the server answers,
see [LSP capabilities](tooling/lsp-capabilities).

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
and launches the `ascript lsp` server. Override the binary in your Zed settings:

```jsonc
{
  "lsp": {
    "ascript": { "binary": { "path": "/absolute/path/to/ascript", "arguments": ["lsp"] } }
  }
}
```

> [!NOTE] Zed loads the tree-sitter grammar from a published repository pinned by
> commit in `editors/zed/extension.toml`. Until the standalone `tree-sitter-ascript`
> grammar repo is published and that commit is pinned, the LSP-driven features (every
> capability on the [capabilities page](tooling/lsp-capabilities)) work, but local
> tree-sitter syntax highlighting falls back to plain text. The language server's
> semantic tokens still color your code.

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
registers the `ascript` parser plus the shared queries under `queries/ascript/`.

> [!NOTE] As with Zed, the tree-sitter parser is pulled from the standalone grammar
> repository; until it is published, register the parser from a local checkout (the
> Neovim README documents the path). LSP features are unaffected.

Formatting: use the LSP (`vim.lsp.buf.format()`), or a
[`conform.nvim`](https://github.com/stevearc/conform.nvim) recipe pointing at
`ascript fmt` as a fallback:

```lua
require("conform").setup({
  formatters = { ascript_fmt = { command = "ascript", args = { "fmt", "$FILENAME" }, stdin = false } },
  formatters_by_ft = { ascript = { "ascript_fmt" } },
})
```

## Performance notes

The server coalesces rapid keystrokes into a single rebuild and supersedes stale
completion/hover results, so editing stays responsive. Very large files degrade
gracefully — above ~256 KiB `semanticTokens/full` is served range-only and inlay hints are
skipped; above ~2 MiB `semanticTokens/full`/inlay/folding/color providers go quiet (but
`semanticTokens/range` is always served, keeping the visible viewport colored) — with a
note in the LSP log. Diagnostics and navigation always run. Initial workspace indexing reports
cancellable work-done progress. (The front-end is a full-reparse design; responsiveness
comes from debouncing, not incremental green-node reuse.)
