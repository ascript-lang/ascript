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

## Tree-sitter highlighting

`require("ascript").setup({})` registers the `ascript` parser and starts highlighting for
`*.as` buffers automatically (via `vim.treesitter.start`). The bundled queries under
`queries/ascript/` are picked up from the runtimepath.

Both nvim-treesitter branches are supported — the plugin detects which is installed:

- **master** (legacy): the parser registers into `get_parser_configs()`; install it with
  `:TSInstall ascript`.
- **main** (the rewrite): the parser registers on the `User TSUpdate` event with the required
  `tier`; install it with `:TSInstall ascript` or `:lua require("nvim-treesitter").install({"ascript"})`.

The parser is pinned to a published grammar commit (see `lua/ascript/treesitter.lua`); bump that
revision when the grammar changes. Highlighting activates once the parser is installed.

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

## Formatting

The **recommended** path is LSP formatting, which works out of the box once the server is
attached:

```lua
:lua vim.lsp.buf.format()
```

or enable format-on-save:

```lua
require("ascript").setup({ format_on_save = true })
```

### conform.nvim (optional)

> **Note on `ascript fmt`:** the CLI formats files **in place** and has **no stdin mode**
> (`ascript fmt -` is not supported and errors out). The conform recipe below therefore uses
> conform's **tempfile** strategy (`stdin = false`, `$FILENAME`): conform writes the buffer to a
> temp file, runs `ascript fmt` on it (which rewrites it in place), and reads the result back.
> Do **not** configure a `stdin = true` / `ascript fmt -` recipe — it will silently fail.

```lua
require("conform").setup({
  formatters = { ascript_fmt = require("ascript.format").conform_formatter },
  formatters_by_ft = { ascript = { "ascript_fmt" } },
})
```

## Upstreaming

The `ascript` server definition is also submitted upstream to
[`nvim-lspconfig`](https://github.com/neovim/nvim-lspconfig) as
`lua/lspconfig/configs/ascript.lua` (PR note: mirror `editors/nvim/lua/ascript/lspconfig.lua`'s
`default_config` exactly — `cmd = {"ascript","lsp"}`, `filetypes = {"ascript"}`, root markers
`ascript.toml`/`.git`). Likewise the tree-sitter parser is submitted to `nvim-treesitter` once the
Phase-5 grammar repo is published.
