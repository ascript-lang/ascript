-- Minimal init that puts this plugin on the runtimepath so a headless nvim can
-- `require("ascript")` and exercise filetype detection without external plugins.
local here = vim.fn.fnamemodify(vim.fn.resolve(vim.fn.expand("<sfile>:p")), ":h")
local plugin_root = vim.fn.fnamemodify(here, ":h") -- editors/nvim
vim.opt.runtimepath:prepend(plugin_root)
vim.opt.swapfile = false
