-- nvim-lspconfig server definition for the AScript language server.
--
-- This registers `ascript` with lspconfig's `configs` table so a user can call
-- `require("lspconfig").ascript.setup({})`. It is also published upstream as an
-- nvim-lspconfig server; see the PR note in the README.

local M = {}

--- The minimum AScript server version this plugin targets. Keep in lockstep with
--- editors/README.md and the VS Code / Zed integrations.
M.min_server_version = "0.6.0"

function M.register()
  local ok, configs = pcall(require, "lspconfig.configs")
  if not ok then
    return false
  end
  local util = require("lspconfig.util")

  if not configs.ascript then
    configs.ascript = {
      default_config = {
        cmd = { "ascript", "lsp" },
        filetypes = { "ascript" },
        root_dir = util.root_pattern("ascript.toml", ".git"),
        single_file_support = true,
        settings = {},
      },
      docs = {
        description = [[
The AScript language server (`ascript lsp`): diagnostics, completion, hover,
navigation, rename, formatting, and semantic tokens. Requires `ascript` on PATH.
]],
      },
    }
  end
  return true
end

--- Convenience: register and `setup` in one call.
--- @param opts table|nil passed through to lspconfig's `setup`.
function M.setup(opts)
  if not M.register() then
    return
  end
  require("lspconfig").ascript.setup(opts or {})
end

return M
