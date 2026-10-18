-- Buffer settings for Kiln (`kiln fmt` style: tabs, width 4).
vim.bo.commentstring = "// %s"
vim.bo.comments = "s1:/*,mb:*,ex:*/,:///,://"
vim.bo.expandtab = false
vim.bo.tabstop = 4
vim.bo.shiftwidth = 4
vim.b.undo_ftplugin = "setl cms< com< et< ts< sw<"

local cfg = require("kiln").config
if cfg.treesitter and pcall(vim.treesitter.language.add, "kiln") then
	pcall(vim.treesitter.start)
	if cfg.indent then
		vim.bo.indentexpr = "v:lua.require'kiln'.indentexpr()"
	end
	if cfg.fold then
		vim.wo[0][0].foldmethod = "expr"
		vim.wo[0][0].foldexpr = "v:lua.vim.treesitter.foldexpr()"
		vim.wo[0][0].foldlevel = 99
	end
end
