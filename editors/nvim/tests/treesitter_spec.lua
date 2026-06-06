-- Regression test for the nvim-treesitter parser registration (both branches).
--
-- Run headless:
--   nvim --headless -u editors/nvim/tests/minimal_init.lua \
--     -c 'luafile editors/nvim/tests/treesitter_spec.lua' -c 'qa'
--
-- It stubs the `nvim-treesitter` plugin modules to simulate each branch and asserts
-- `require("ascript.treesitter").register()` takes the correct path. Exits non-zero on
-- failure (via `:cq`) so CI catches a regression — e.g. a re-introduced master-only API.

-- Make `require("ascript.treesitter")` resolvable even without runtimepath wiring.
package.path = "editors/nvim/lua/?.lua;editors/nvim/lua/?/init.lua;" .. package.path

local fails = 0
local function check(name, cond)
  print((cond and "  ok   " or "  FAIL ") .. name)
  if not cond then fails = fails + 1 end
end

-- ---- main branch: no get_parser_configs() ----
package.loaded["ascript.treesitter"] = nil
package.loaded["nvim-treesitter.parsers"] = {}
package.loaded["nvim-treesitter"] = { install = function() end }
local okm = require("ascript.treesitter").register()
local p = package.loaded["nvim-treesitter.parsers"]
print("[nvim-treesitter main branch]")
check("register() returned true", okm == true)
check("parsers.ascript registered", type(p.ascript) == "table")
check("tier is set (mandatory on main)", p.ascript and p.ascript.tier ~= nil)
check("install_info.url -> tree-sitter-ascript", p.ascript and p.ascript.install_info
  and p.ascript.install_info.url:match("tree%-sitter%-ascript") ~= nil)
check("install_info.revision pinned", p.ascript and p.ascript.install_info.revision ~= nil)
check("no master-only 'files' key leaked", p.ascript and p.ascript.install_info.files == nil)
check("TSUpdate register autocmd created",
  #vim.api.nvim_get_autocmds({ group = "ascript_treesitter_register", event = "User" }) > 0)
check("FileType highlight autocmd created",
  #vim.api.nvim_get_autocmds({ group = "ascript_treesitter_highlight", event = "FileType" }) > 0)

-- ---- master branch: has get_parser_configs() ----
package.loaded["ascript.treesitter"] = nil
local store = {}
package.loaded["nvim-treesitter.parsers"] = { get_parser_configs = function() return store end }
local okM = require("ascript.treesitter").register()
print("[nvim-treesitter master branch]")
check("register() returned true", okM == true)
check("parser_config.ascript registered", type(store.ascript) == "table")
check("legacy install_info.files = {src/parser.c}", store.ascript and store.ascript.install_info
  and store.ascript.install_info.files and store.ascript.install_info.files[1] == "src/parser.c")
check("legacy filetype key set", store.ascript and store.ascript.filetype == "ascript")

-- ---- nvim-treesitter not installed ----
package.loaded["ascript.treesitter"] = nil
package.loaded["nvim-treesitter.parsers"] = nil
package.preload["nvim-treesitter.parsers"] = function() error("not installed") end
print("[nvim-treesitter absent]")
check("register() returns false gracefully", require("ascript.treesitter").register() == false)

print(fails == 0 and "ALL PASS" or ("FAILURES: " .. fails))
if fails > 0 then
  vim.cmd("cq") -- non-zero exit for CI
end
