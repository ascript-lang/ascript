-- nvim-lspconfig server definition for the AScript language server.
--
-- This registers `ascript` with lspconfig's `configs` table so a user can call
-- `require("lspconfig").ascript.setup({})`. It is also published upstream as an
-- nvim-lspconfig server; see the PR note in the README.

local M = {}

--- The minimum AScript server version this plugin targets. Keep in lockstep with
--- editors/README.md and the VS Code / Zed integrations.
M.min_server_version = "0.6.0"

--- Resolve `ascript` to an absolute path. Tries the inherited PATH first (`exepath`),
--- then common install dirs a GUI-launched Neovim's PATH often omits — the classic macOS
--- "app started from the Dock has a stripped PATH" problem, where a binary in ~/.local/bin
--- or ~/.cargo/bin is invisible. Falls back to the bare name "ascript" so a genuinely
--- missing binary still surfaces lspconfig's normal "executable not found" error.
--- @param candidates string[]|nil override list (for tests)
--- @return string command (absolute path, or "ascript")
function M._resolve_cmd(candidates)
  local exe = vim.fn.exepath("ascript")
  if exe ~= nil and exe ~= "" then
    return exe
  end
  if not candidates then
    local home = vim.fn.expand("~")
    candidates = {
      home .. "/.local/bin/ascript",
      home .. "/.cargo/bin/ascript",
      home .. "/bin/ascript",
      "/usr/local/bin/ascript",
      "/opt/homebrew/bin/ascript",
      "/usr/bin/ascript",
    }
  end
  for _, c in ipairs(candidates) do
    if vim.fn.executable(c) == 1 then
      return c
    end
  end
  return "ascript"
end

function M.register()
  local ok, configs = pcall(require, "lspconfig.configs")
  if not ok then
    return false
  end
  local util = require("lspconfig.util")

  if not configs.ascript then
    configs.ascript = {
      default_config = {
        cmd = { M._resolve_cmd(), "lsp" },
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
