# Installation

There are no release binaries yet. Kiln builds from source with a Rust toolchain.

## Requirements

- [Rust](https://rustup.rs) 1.82 or newer (`rustup` installs `cargo`)
- `git`
- A C toolchain for linking compiled programs: `cc` (gcc or clang) on Linux and macOS (`xcode-select --install`), or the Visual Studio C++ build tools on Windows
- Optional: Node.js 18+ for the editor grammars, and [mdBook](https://rust-lang.github.io/mdBook/) to build this site

## Build the compiler

```sh
git clone https://github.com/Cody-will/kiln.git
cd kiln
cargo build --release
```

This produces `target/release/kiln`. Put it on your `PATH` or install it with Cargo:

```sh
cargo install --path crates/kiln_cli
kiln --version
```

## Check that it works

```sh
kiln check examples/hello.kn
# examples/hello.kn: ok
kiln run examples/hello.kn
# Hello, Kiln!
```

The `kiln` binary has these commands today:

| Command | What it does |
|---|---|
| `kiln lex <file.kn>` | Print the token stream (including inserted semicolons) |
| `kiln parse <file.kn>` | Print the syntax tree |
| `kiln check <file.kn>` | Resolve names, type-check, and borrow-check, printing diagnostics |
| `kiln build <file.kn> [-o out]` | Compile to a native executable |
| `kiln run <file.kn>` | Compile and run |
| `kiln new <name> [--lib]` | Create a package |
| `kiln test [paths...]` | Run `@test` functions |
| `kiln fmt [--check] [paths...]` | Format source files |

<div class="status">
<code>kiln build</code>, <code>kiln run</code>, <code>kiln test</code>, and <code>kiln check</code> work on single files and on packages (<code>kiln new</code>; see <a href="tutorial/10-packages.html">Packages and modules</a>).
</div>

## Editor support

- **VS Code:** `editors/vscode` contains an extension with a TextMate grammar for `.kn` files. Install it locally with `cd editors/vscode && npx @vscode/vsce package` and then *Extensions → Install from VSIX*. See its README.
- **Neovim, Helix, Zed, and others:** `editors/tree-sitter-kiln` is a Tree-sitter grammar with highlight queries.

## Running the test suite

```sh
cargo test                 # compiler tests
cargo clippy --all-targets # lints
cargo fmt --check          # formatting
```
