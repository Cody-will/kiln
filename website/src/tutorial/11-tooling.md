# Tooling

Kiln ships one `kiln` command for everything, like `go` and `cargo`.

## Available today

| Command | Description |
|---|---|
| `kiln check <file.kn>` | Parse, resolve names, and type-check. Exit status 1 if there are errors. |
| `kiln parse <file.kn>` | Print the syntax tree (for debugging the compiler). |
| `kiln lex <file.kn>` | Print tokens, including the semicolons inserted at line ends. |
| `kiln build <file.kn> [-o out]` | Compile to a native executable (needs a C linker: `cc`, or the Visual Studio build tools on Windows). |
| `kiln run <file.kn> [args...]` | Build into a temporary directory and run; exits with the program's status. |
| `kiln mir <file.kn>` | Print the mid-level IR (for debugging the compiler). |
| `kiln --version` | Print the version. |

Diagnostics look like this:

```text
error: mismatched types: expected `i32`, found `bool`
 --> app.kn:2:9
  |
2 | 	return true
  | 	       ^^^^
```

## Editors

- **VS Code:** syntax highlighting, bracket matching, comment toggling, and auto-indent through `editors/vscode`.
- **Tree-sitter editors** (Neovim, Helix, Zed): `editors/tree-sitter-kiln` provides a grammar and `highlights.scm`.

## Planned

| Command | Description |
|---|---|
| 🚧 `kiln new <name>` | Create a package with `kiln.toml` and `src/main.kn` |
| 🚧 `kiln test` | Run `#[test]` functions and the `tests/` directory |
| 🚧 `kiln fmt` | The canonical formatter: tabs, Go-style braces, trailing commas on multi-line lists, and no configuration |
| 🚧 `kiln lint`, `kiln doc`, `kiln bench` | Linting, documentation, benchmarks |
| 🚧 `kiln add`, `kiln publish` | Dependency management and the package registry |
| 🚧 `kiln-ls` | A language server for diagnostics, hover, go-to-definition, completion, and rename |

## Formatting style

`kiln fmt` will have exactly one style, and all code in this book follows it:

- Indent with tabs.
- Put the opening brace on the same line.
- Multi-line lists end with a trailing comma; single-line lists don't.
- No semicolons at line ends.

Because there are no options, every Kiln codebase looks the same.
