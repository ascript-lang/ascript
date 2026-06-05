# ascript.nvim

AScript language support for Neovim (>= 0.10): LSP, tree-sitter syntax, filetype detection,
and formatting.

## Requirements

- `ascript` on your `PATH` (>= 0.6.0)
- [`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig)
- optional: [`nvim-treesitter`](https://github.com/nvim-treesitter/nvim-treesitter),
  [`conform.nvim`](https://github.com/stevearc/conform.nvim)

## Install (lazy.nvim)

```lua
{
  "ascript-lang/ascript.nvim",
  dependencies = { "neovim/nvim-lspconfig" },
  config = function()
    require("ascript").setup({})
  end,
}
```

## Without the plugin — a one-file snippet

If you do not want a plugin, register the server directly:

```lua
local configs = require("lspconfig.configs")
local util = require("lspconfig.util")
if not configs.ascript then
  configs.ascript = {
    default_config = {
      cmd = { "ascript", "lsp" },
      filetypes = { "ascript" },
      root_dir = util.root_pattern("ascript.toml", ".git"),
      single_file_support = true,
    },
  }
end
vim.filetype.add({ extension = { as = "ascript" } })
require("lspconfig").ascript.setup({})
```

## Upstreaming

The `ascript` server definition is also submitted upstream to
[`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig) as
`lua/lspconfig/configs/ascript.lua` (PR note: mirror `editors/nvim/lua/ascript/lspconfig.lua`'s
`default_config` exactly — `cmd = {"ascript","lsp"}`, `filetypes = {"ascript"}`, root markers
`ascript.toml`/`.git`). Likewise the tree-sitter parser is submitted to `nvim-treesitter` once the
Phase-5 grammar repo is published.
