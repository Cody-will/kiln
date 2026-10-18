# Editors

Kiln's editor support has two parts:
- **Highlighting**: a TextMate grammar in VS Code, and tree-sitter-kiln in Neovim.
- **Language features** from `kiln lsp`: diagnostics, hover, go to definition, references, rename, completion, document symbols, formatting with `kiln fmt`, and inlay type hints.

Both editors start `kiln lsp` themselves; `kiln` must be on `PATH` (or configured, below).

## VS Code

The installer ships the extension in `~/.kiln/lib/kiln/editors/kiln.vsix`. Install it with:

```sh
kiln editor install vscode
```

This runs `code --install-extension` on the bundled `.vsix` (it also tries `codium`, `code-insiders` and `code-oss`; `--code=<cli>` picks one). By hand:

```sh
code --install-extension ~/.kiln/lib/kiln/editors/kiln.vsix
```

Open a `.kn` file: it is highlighted, and the "Kiln Language Server" output channel shows the server starting. Settings:

| Setting | Default | |
|---|---|---|
| `kiln.server.path` | `kiln` | the `kiln` executable that runs `kiln lsp` |

**Kiln: Restart Language Server** restarts it. The extension also sets tab indentation (width 4) and makes `kiln fmt` the formatter for Kiln files.

From a clone, `make vsix` builds `editors/vscode/kiln-<version>.vsix`. `npm test` in `editors/vscode` checks the grammar. `KILN=$(which kiln) xvfb-run -a npm run test:e2e` runs an end-to-end test that launches VS Code, opens a file with a type error, and waits for the diagnostic and a hover from `kiln lsp`.

## Neovim

The plugin, in `editors/nvim`, needs Neovim 0.11 or newer for the built-in LSP client (`vim.lsp.config`). On older versions it uses nvim-lspconfig if that is installed. It:
- registers the `kiln` filetype for `.kn` files, and sets tab indentation (width 4) and `//` comments;
- tells nvim-treesitter how to build the tree-sitter-kiln parser (`:TSInstall kiln`), and ships its highlight, indent and fold queries in `queries/kiln`;
- enables `kiln lsp` for Kiln buffers, with `kiln.toml` (or `.git`) marking the project root.

### With lazy.nvim

The installer already put the plugin in `~/.kiln/lib/kiln/editors/nvim`:

```lua
{
  dir = vim.fn.expand("~/.kiln/lib/kiln/editors/nvim"),
  name = "kiln.nvim",
  lazy = false,
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  opts = {},
}
```

Or straight from the repository, adding its `editors/nvim` subdirectory to the runtime path:

```lua
{
  "Cody-will/kiln",
  name = "kiln.nvim",
  lazy = false,
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  config = function(plugin)
    vim.opt.rtp:append(plugin.dir .. "/editors/nvim")
    require("kiln").setup({})
  end,
}
```

Then run `:TSInstall kiln` once. nvim-treesitter's `main` branch builds parsers with the `tree-sitter` CLI (`pacman -S tree-sitter-cli` or `npm i -g tree-sitter-cli`) and a C compiler. The `master` branch needs only a C compiler.

### Without a plugin manager

```sh
kiln editor install nvim
```

This copies the plugin into `~/.local/share/nvim/site/pack/kiln/start/kiln.nvim`, where Neovim loads it at startup. The Arch package does the same system-wide.

### Options

`setup()` is optional; these are the defaults:

```lua
require("kiln").setup({
  lsp = true,                 -- start kiln lsp for .kn buffers
  cmd = { "kiln", "lsp" },    -- e.g. { vim.fn.expand("~/.kiln/bin/kiln"), "lsp" }
  treesitter = true,          -- highlight with the kiln parser when installed
  indent = true,              -- tree-sitter indentation
  fold = true,                -- tree-sitter folding (foldlevel 99)
  parser_path = nil,          -- build the parser from a local editors/tree-sitter-kiln
})
```

With `vim.lsp.config` you can also change the server config directly: `vim.lsp.config("kiln", { settings = ... })`.

### Testing

`editors/nvim/test/run.sh [kiln]` runs Neovim headless on a sample file. It checks the filetype, highlight captures at known positions, the indent and fold queries, and that `kiln lsp` attaches and reports the sample's type error. `make editors-test` runs this and the VS Code grammar test.

## Other editors

Any editor with an LSP client can run `kiln lsp` (stdio) for `*.kn` files. Tree-sitter-based editors (Helix, Zed) can use `editors/tree-sitter-kiln` and its `queries/`.
