# tree-sitter-kiln

This is a [Tree-sitter](https://tree-sitter.github.io/) grammar for Kiln. Editors such as Neovim, Helix, and Zed, as well as GitHub, use it for syntax highlighting, code folding, and structural navigation.

- **`grammar.js`:** the grammar. It covers every item, statement, expression, pattern, and type form in `docs/SPEC.md`, including `:=` / `x mut :=`, `spawn`, `select`, `chan<- T` / `<-chan T`, and string interpolation.
- **`queries/highlights.scm`:** the highlight captures, using the standard capture names (`@keyword`, `@type`, `@function.call`, and so on).
- **`test/corpus/`:** corpus tests.
- **`src/`:** the generated parser. Regenerate it with `tree-sitter generate`. It is committed so the grammar can be used without the CLI.

## Development

```sh
npm install                 # installs tree-sitter-cli
npx tree-sitter generate    # regenerate src/ from grammar.js
npx tree-sitter test        # run the corpus tests
npx tree-sitter parse --quiet ../../examples/*.kn   # every example must parse without ERROR nodes
```

## How newlines work

Statements end with `;` or a newline, as in Go. The grammar follows the same approach as `tree-sitter-go`:

- A newline counts as a statement terminator wherever the parser expects one.
- Everywhere else, a newline is skipped as whitespace.

This grammar is meant for highlighting and editor tooling, so it is somewhat more permissive than the compiler's parser (`crates/kiln_parser`). The compiler's parser is the reference.

## Using it in Neovim (nvim-treesitter)

```lua
local parser_config = require("nvim-treesitter.parsers").get_parser_configs()
parser_config.kiln = {
  install_info = {
    url = "https://github.com/Cody-will/kiln",
    location = "editors/tree-sitter-kiln",
    files = { "src/parser.c" },
    branch = "main",
  },
  filetype = "kn",
}
vim.filetype.add({ extension = { kn = "kiln" } })
```

Then copy `queries/highlights.scm` to `~/.config/nvim/queries/kiln/highlights.scm`.
