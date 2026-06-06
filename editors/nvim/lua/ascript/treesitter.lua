-- nvim-treesitter parser registration for AScript.
--
-- Points at the promoted standalone tree-sitter-ascript grammar. The bundled queries
-- under editors/nvim/queries/ascript/ are picked up from Neovim's runtimepath once this
-- plugin is on the path — that works identically on both nvim-treesitter branches.
--
-- Supports BOTH nvim-treesitter branches:
--   * master (legacy): require("nvim-treesitter.parsers").get_parser_configs() + install_info
--   * main   (rewrite): parsers.ascript = { install_info = {...}, tier = N } registered on the
--                       `User TSUpdate` event (the master setup{}/module system was removed).
-- Highlighting is started with the core `vim.treesitter.start()` API in a FileType autocmd,
-- which works on either branch (main dropped the `highlight = { enable = true }` module).

local M = {}

local GRAMMAR_URL = "https://github.com/ascript-lang/tree-sitter-ascript"
-- Pinned to the published grammar commit for reproducibility; bump on grammar updates.
local GRAMMAR_REV = "a075a12ad120e21fc6df6b5e6b7f4ff40fd99c74"

-- Start tree-sitter highlighting for AScript buffers via core Neovim APIs. Branch-agnostic,
-- and a harmless no-op (pcall) until the `ascript` parser is actually installed.
local function enable_highlight()
  vim.api.nvim_create_autocmd("FileType", {
    pattern = "ascript",
    group = vim.api.nvim_create_augroup("ascript_treesitter_highlight", { clear = true }),
    callback = function(args)
      pcall(vim.treesitter.start, args.buf, "ascript")
    end,
  })
end

function M.register()
  local ok, parsers = pcall(require, "nvim-treesitter.parsers")
  if not ok then
    return false
  end

  if type(parsers.get_parser_configs) == "function" then
    -- master branch (legacy API): mutate the shared parser-config table directly.
    local parser_config = parsers.get_parser_configs()
    parser_config.ascript = {
      install_info = {
        url = GRAMMAR_URL,
        files = { "src/parser.c" },
        revision = GRAMMAR_REV,
        generate_requires_npm = false,
        requires_generate_from_grammar = false,
      },
      filetype = "ascript",
    }
  else
    -- main branch (rewrite). Parser configs are keyed directly on the parsers module and
    -- must be (re-)applied on the `User TSUpdate` event; `tier` is mandatory (2 = community).
    local function apply()
      local p = require("nvim-treesitter.parsers")
      p.ascript = {
        install_info = {
          url = GRAMMAR_URL,
          revision = GRAMMAR_REV,
        },
        tier = 2,
      }
    end
    -- Apply now (so `:TSInstall ascript` / `install({"ascript"})` can resolve it) and on
    -- every TSUpdate (so it survives nvim-treesitter rebuilding its parser table).
    apply()
    vim.api.nvim_create_autocmd("User", {
      pattern = "TSUpdate",
      group = vim.api.nvim_create_augroup("ascript_treesitter_register", { clear = true }),
      callback = apply,
    })
    -- Map the `ascript` filetype to the `ascript` language for the core tree-sitter APIs.
    pcall(vim.treesitter.language.register, "ascript", "ascript")
  end

  enable_highlight()
  return true
end

return M
