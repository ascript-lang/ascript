-- Regression test for the `ascript` binary resolver in lspconfig.lua.
--
-- Run headless:
--   nvim --headless -u editors/nvim/tests/minimal_init.lua \
--     -c 'luafile editors/nvim/tests/lspconfig_spec.lua' -c 'qa'
--
-- Guards the macOS GUI-PATH fix: a binary in ~/.local/bin (etc.) that the inherited
-- PATH omits must still be found via the fallback search. Exits non-zero on failure.

package.path = "editors/nvim/lua/?.lua;editors/nvim/lua/?/init.lua;" .. package.path
local M = require("ascript.lspconfig")

local fails = 0
local function check(name, cond)
  print((cond and "  ok   " or "  FAIL ") .. name)
  if not cond then fails = fails + 1 end
end

-- A fake executable in a temp dir that is NOT on PATH.
local tmp = vim.fn.tempname()
vim.fn.mkdir(tmp, "p")
local fake = tmp .. "/ascript"
vim.fn.writefile({ "#!/bin/sh", "true" }, fake)
vim.fn.setfperm(fake, "rwxr-xr-x")

check("_resolve_cmd is exposed", type(M._resolve_cmd) == "function")
-- With the binary off PATH, the inherited-PATH lookup misses it; the fallback list finds it.
check("fallback finds a binary the PATH omits (the ~/.local/bin case)", M._resolve_cmd({ fake }) == fake)
check("returns bare 'ascript' when no candidate exists (error still surfaces)",
  M._resolve_cmd({ "/no/such/dir/ascript" }) == "ascript")

print(fails == 0 and "ALL PASS" or ("FAILURES: " .. fails))
if fails > 0 then
  vim.cmd("cq")
end
