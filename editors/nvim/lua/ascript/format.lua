-- Formatting for AScript.
--
-- Primary (recommended, works unconditionally): LSP `textDocument/formatting`
-- (the server formats via the same engine as `ascript fmt`). Use M.format_buffer
-- or M.enable_format_on_save.
--
-- Optional: a conform.nvim recipe. NOTE: the `ascript fmt` CLI does NOT read from
-- stdin (there is no `ascript fmt -` mode) — it formats files IN PLACE. conform's
-- `tempfile` strategy fits this exactly: with `stdin = false`, conform writes the
-- buffer to a temp file, runs `ascript fmt <tempfile>` (which rewrites it in place),
-- then reads the rewritten file back. So the recipe below uses `stdin = false` and
-- `"$FILENAME"`. Do NOT set `stdin = true` / `args = { "fmt", "-" }` — that path
-- silently fails (`ascript fmt -` errors with "could not read -").

local M = {}

--- A conform.nvim formatter spec pointing at `ascript fmt`.
---
--- Uses conform's tempfile strategy (`stdin = false`): `ascript fmt` formats files
--- in place, so conform writes the buffer to a temp file, runs `ascript fmt` on it,
--- and reads the formatted result back. `ascript fmt` has no stdin mode, so this
--- recipe deliberately avoids `stdin = true`.
---
--- Usage in a user's conform setup:
---   require("conform").setup({
---     formatters = { ascript_fmt = require("ascript.format").conform_formatter },
---     formatters_by_ft = { ascript = { "ascript_fmt" } },
---   })
M.conform_formatter = {
  command = "ascript",
  args = { "fmt", "$FILENAME" },
  stdin = false,
  -- `ascript fmt <file>` rewrites the file in place; conform reads it back.
}

--- Format the current buffer via the LSP (the recommended, always-available path).
--- Requires the `ascript` language server to be attached.
function M.format_buffer()
  vim.lsp.buf.format({ async = false })
end

--- Optionally enable LSP format-on-save for ascript buffers.
function M.enable_format_on_save()
  vim.api.nvim_create_autocmd("BufWritePre", {
    pattern = "*.as",
    callback = function()
      vim.lsp.buf.format({ async = false })
    end,
  })
end

return M
