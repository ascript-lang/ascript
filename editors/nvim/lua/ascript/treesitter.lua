-- nvim-treesitter parser registration for AScript.
--
-- Points at the promoted standalone tree-sitter-ascript grammar (LSP Phase 5).
-- The bundled queries under editors/nvim/queries/ascript/ are picked up by Neovim's
-- runtimepath once this plugin is on the path.

local M = {}

function M.register()
  local ok, parsers = pcall(require, "nvim-treesitter.parsers")
  if not ok then
    return false
  end
  local parser_config = parsers.get_parser_configs()
  parser_config.ascript = {
    install_info = {
      url = "https://github.com/ascript-lang/tree-sitter-ascript",
      files = { "src/parser.c" },
      -- Pinned to the published grammar commit for reproducibility; bump on grammar updates.
      revision = "a075a12ad120e21fc6df6b5e6b7f4ff40fd99c74",
      generate_requires_npm = false,
      requires_generate_from_grammar = false,
    },
    filetype = "ascript",
  }
  return true
end

return M
