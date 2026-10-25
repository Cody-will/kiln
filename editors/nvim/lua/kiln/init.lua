-- Kiln for Neovim. Works without setup(); call it only to change defaults:
--
--   require("kiln").setup({
--     lsp = true,             -- start `kiln lsp` for .kn buffers (nvim 0.11+)
--     cmd = { "kiln", "lsp" },
--     treesitter = true,      -- highlight with the kiln parser when installed
--     indent = true,          -- tree-sitter indentation
--     fold = true,            -- tree-sitter folding
--     parser_path = nil,      -- build the parser from a local checkout's
--                             -- editors/tree-sitter-kiln instead of GitHub
--   })
local M = {}

M.config = { lsp = true, cmd = { "kiln", "lsp" }, treesitter = true, indent = true, fold = true, parser_path = nil }

M.parser = {
	url = "https://github.com/Cody-will/kiln",
	location = "editors/tree-sitter-kiln",
	files = { "src/parser.c" },
	branch = "main",
}

-- Tells nvim-treesitter how to build the parser (`:TSInstall kiln`).
-- Both the `master` API (get_parser_configs) and the `main` one
-- (`require("nvim-treesitter.parsers")` table, refreshed on TSUpdate).
local function register_parser()
	local ok, parsers = pcall(require, "nvim-treesitter.parsers")
	if not ok then
		return
	end
	local local_path = M.config.parser_path
	if type(parsers.get_parser_configs) == "function" then
		local info = local_path and { url = local_path, files = { "src/parser.c" } } or M.parser
		parsers.get_parser_configs().kiln = { install_info = info, filetype = "kiln" }
	else
		local function add()
			local info = vim.tbl_extend("force", M.parser, { queries = "editors/nvim/queries/kiln" })
			if M.config.parser_path then
				info = { path = M.config.parser_path, queries = "queries" }
			end
			require("nvim-treesitter.parsers").kiln = {
				install_info = info,
				tier = 2,
			}
		end
		add()
		vim.api.nvim_create_autocmd("User", { pattern = "TSUpdate", callback = add })
	end
end

local function lsp_config()
	return {
		cmd = M.config.cmd,
		filetypes = { "kiln" },
		root_markers = { "kiln.toml", ".git" },
	}
end

local function register_lsp()
	if not M.config.lsp then
		return
	end
	if vim.lsp.config and vim.lsp.enable then
		vim.lsp.config("kiln", lsp_config())
		vim.lsp.enable("kiln")
		return
	end
	-- Older nvim: nvim-lspconfig, if installed.
	local ok, configs = pcall(require, "lspconfig.configs")
	if not ok then
		return
	end
	if not configs.kiln then
		configs.kiln = {
			default_config = {
				cmd = M.config.cmd,
				filetypes = { "kiln" },
				root_dir = require("lspconfig.util").root_pattern("kiln.toml", ".git"),
			},
		}
	end
	require("lspconfig").kiln.setup({})
end

local registered = false
function M.register()
	if registered then
		return
	end
	registered = true
	vim.filetype.add({ extension = { kn = "kiln" } })
	pcall(vim.treesitter.language.register, "kiln", "kiln")
	register_parser()
	register_lsp()
end

function M.setup(opts)
	local was_lsp = M.config.lsp
	M.config = vim.tbl_deep_extend("force", M.config, opts or {})
	if registered and was_lsp and not M.config.lsp and vim.lsp.enable then
		vim.lsp.enable("kiln", false)
	end
	if registered and M.config.lsp and vim.lsp.config then
		vim.lsp.config("kiln", lsp_config())
		vim.lsp.enable("kiln")
	end
	if registered then
		register_parser()
	end
	M.register()
end

-- Tree-sitter indentation: nvim-treesitter's when present, else cindent-ish
-- fallback via the builtin.
function M.indentexpr()
	local ok, ind = pcall(require, "nvim-treesitter.indent")
	if ok and ind.get_indent then
		return ind.get_indent(vim.v.lnum)
	end
	ok, ind = pcall(require, "nvim-treesitter")
	if ok and ind.indentexpr then
		return ind.indentexpr()
	end
	return vim.fn.cindent(vim.v.lnum)
end

return M
