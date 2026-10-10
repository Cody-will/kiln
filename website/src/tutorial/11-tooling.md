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
| `kiln test [paths...]` | Run `#[test]` functions in the given files, or in every `.kn` file under the given directories (default: the current one). `--run <text>` picks tests by name, `--ignored` runs `#[ignore]` tests, `--timeout <secs>` changes the 10-minute limit. |
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

## Testing

Write tests next to your code as functions marked `#[test]`:

```kiln
fn add(a: i64, b: i64) -> i64 {
	return a + b
}

#[test]
fn adds() {
	assert_eq(add(2, 2), 4)
}

#[test]
fn parses() -> Result<(), std::strconv::ParseError> {
	assert_eq(i64::parse("42")?, 42)
	return Ok(())
}

#[test]
#[should_panic(expected = "divide by zero")]
fn divides_by_zero() {
	z := 0
	println("{10 / z}")
}
```

```text
$ kiln test math.kn

running 3 tests in math.kn
test adds ... ok
test parses ... ok
test divides_by_zero ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 filtered out; finished in 0.41s
```

Each test runs in its own process, in parallel, so a panic fails only that test. A test's output is shown only when it fails. A test that returns `Err` fails and prints the error. `#[ignore]` skips a slow test unless you pass `--ignored`.

## Editors

- **VS Code:** `editors/vscode` adds highlighting and editing support and starts the language server.
- **Language server:** `kiln lsp` speaks the Language Server Protocol over stdio, so any LSP editor can use it (Neovim: `cmd = { "kiln", "lsp" }`). It provides diagnostics as you type, hover (types, signatures, docs), go to definition, find references, rename, completion (locals, items, keywords, `.` members), the document outline, formatting through `kiln fmt`, and inlay type hints on `x := ...`. It works on one file at a time, like the rest of the toolchain today.
- **Tree-sitter editors** (Neovim, Helix, Zed): `editors/tree-sitter-kiln` provides a grammar and `highlights.scm`.

## Planned

| Command | Description |
|---|---|
| 🚧 `kiln new <name>` | Create a package with `kiln.toml` and `src/main.kn` |
| 🚧 `kiln lint`, `kiln doc`, `kiln bench` | Linting, documentation, benchmarks |
| 🚧 `kiln add`, `kiln publish` | Dependency management and the package registry |

## Formatting: `kiln fmt`

`kiln fmt` rewrites files in the one canonical style, like `gofmt`:

```sh
kiln fmt                 # every .kn file under the current directory
kiln fmt src/main.kn     # one file (or a directory)
kiln fmt --check         # change nothing; list unformatted files, exit 1 if any (for CI)
kiln fmt --stdin < a.kn  # standard input to standard output (for editors)
```

It keeps your line breaks and comments and normalizes everything else. Files with syntax errors are reported and left alone. The style, which all code in this book follows:

- Indent with tabs.
- Put the opening brace on the same line.
- Multi-line lists end with a trailing comma; single-line lists don't.
- No semicolons at line ends.
- Spaces around binary operators, none after `(` or before `,`; `S { x: 1 }` but `std::io::{Read, Write}`.
- A continued line (`.method()` chains, a line ending in an operator) is indented one more tab.
- At most one blank line in a row, and one between top-level items.
- Consecutive `import` lines are sorted (std first) and deduplicated.
- `//comment` becomes `// comment`.

Where tokens alone can't tell (`a<b` could be generics), `kiln fmt` keeps your choice of spacing: write `a < b` and it stays.

Because there are no options, every Kiln codebase looks the same.
