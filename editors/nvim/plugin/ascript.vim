" ascript.nvim guard: ensure ftdetect runs even before setup() is called.
if exists('g:loaded_ascript')
  finish
endif
let g:loaded_ascript = 1

lua require('ascript.lspconfig').register()
