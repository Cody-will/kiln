-- Kiln support: the `.kn` filetype, the tree-sitter-kiln parser for
-- nvim-treesitter, and the `kiln lsp` language server.
if vim.g.loaded_kiln then
	return
end
vim.g.loaded_kiln = true
require("kiln").register()
