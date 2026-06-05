-- ascript.nvim — minimal Neovim integration for AScript.
--
-- `require("ascript").setup({...})` wires:
--   * filetype detection (also handled by ftdetect/ for lazy plugins),
--   * the AScript language server via nvim-lspconfig,
--   * the nvim-treesitter parser registration,
--   * optional format-on-save.

local M = {}

M.min_server_version = "0.6.0"

local defaults = {
  -- Passed through to lspconfig's `setup` (on_attach, capabilities, ...).
  lsp = {},
  -- Register the treesitter parser config (requires nvim-treesitter).
  treesitter = true,
  -- Enable LSP format-on-save for *.as.
  format_on_save = false,
}

--- @param opts table|nil
function M.setup(opts)
  opts = vim.tbl_deep_extend("force", defaults, opts or {})

  -- Ensure *.as is detected even when this plugin is lazy-loaded.
  vim.filetype.add({ extension = { as = "ascript" } })

  require("ascript.lspconfig").setup(opts.lsp)

  if opts.treesitter then
    pcall(function()
      require("ascript.treesitter").register()
    end)
  end

  if opts.format_on_save then
    require("ascript.format").enable_format_on_save()
  end
end

return M
