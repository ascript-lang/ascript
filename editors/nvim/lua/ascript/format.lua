-- Formatting for AScript.
--
-- Primary: LSP `textDocument/formatting` (the server formats via `ascript fmt`'s engine).
-- Fallback / explicit: a conform.nvim recipe shelling out to `ascript fmt -` (stdin → stdout).

local M = {}

--- A conform.nvim formatter spec pointing at `ascript fmt`.
--- Usage in a user's conform setup:
---   require("conform").setup({
---     formatters = { ascript_fmt = require("ascript.format").conform_formatter },
---     formatters_by_ft = { ascript = { "ascript_fmt" } },
---   })
M.conform_formatter = {
  command = "ascript",
  args = { "fmt", "-" },
  stdin = true,
  -- `ascript fmt -` formats stdin and writes the result to stdout.
}

--- Format the current buffer via the LSP if a client is attached; otherwise no-op
--- (a user may instead wire conform.nvim with M.conform_formatter).
function M.format_buffer()
  vim.lsp.buf.format({ async = false })
end

--- Optionally enable format-on-save via the LSP for ascript buffers.
function M.enable_format_on_save()
  vim.api.nvim_create_autocmd("BufWritePre", {
    pattern = "*.as",
    callback = function()
      vim.lsp.buf.format({ async = false })
    end,
  })
end

return M
