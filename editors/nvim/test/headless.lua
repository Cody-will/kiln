local failures = 0
local function check(ok, what)
	print((ok and "ok   " or "FAIL ") .. what)
	if not ok then
		failures = failures + 1
	end
end

require("kiln").setup({ cmd = { vim.env.KILN_TEST_BIN, "lsp" } })
vim.cmd("edit!")
check(vim.bo.filetype == "kiln", "filetype is kiln (" .. vim.bo.filetype .. ")")
check(vim.treesitter.highlighter.active[vim.api.nvim_get_current_buf()] ~= nil, "tree-sitter highlighter active")

local function caps(row, col)
	local t = {}
	for _, c in ipairs(vim.treesitter.get_captures_at_pos(0, row, col)) do
		t[#t + 1] = c.capture
	end
	return table.concat(t, ",")
end
local function has(row, col, want)
	local c = caps(row, col)
	check(("," .. c .. ","):find("," .. want .. ",", 1, true) ~= nil, ("(%d,%d) has @%s [%s]"):format(row, col, want, c))
end
has(0, 0, "comment")
has(1, 0, "keyword")
has(1, 4, "function")
has(3, 1, "keyword.modifier")
has(3, 7, "type.builtin")
has(3, 14, "string")
has(4, 1, "function.call")

-- Indent of the line after `fun main() {` is one level.
local indent = vim.treesitter.query.get("kiln", "indents") ~= nil
check(indent, "indents query loads")
check(vim.treesitter.query.get("kiln", "folds") ~= nil, "folds query loads")
check(vim.wo.foldmethod == "expr", "foldmethod=expr")
check(vim.fn.foldlevel(3) >= 1, "line 3 is inside a fold (" .. vim.fn.foldlevel(3) .. ")")

local ok = vim.wait(20000, function()
	return #vim.diagnostic.get(0) > 0
end, 100)
local clients = vim.lsp.get_clients({ bufnr = 0, name = "kiln" })
check(#clients == 1, "kiln lsp attached")
check(ok, "diagnostics from kiln lsp")
for _, d in ipairs(vim.diagnostic.get(0)) do
	print("     line " .. (d.lnum + 1) .. ": " .. d.message)
end
print(failures == 0 and "NVIM OK" or ("NVIM FAILED " .. failures))
vim.cmd(failures == 0 and "qa!" or "cq!")
