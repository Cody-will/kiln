# Installation

There are no release binaries yet. The installer builds Kiln from source and installs it like other toolchains, into `~/.kiln`.

## Requirements

- [Rust](https://rustup.rs) 1.82 or newer (`rustup` installs `cargo`)
- `git`
- A C toolchain for linking compiled programs: `cc` (gcc or clang) on Linux and macOS (`xcode-select --install`), or the Visual Studio C++ build tools on Windows
- Optional: LLVM 18 for the optimizing `--release` backend (`apt install llvm-18-dev` or `brew install llvm@18`). The installer uses it when it finds it.

## Install

```sh
git clone https://github.com/Cody-will/kiln.git
cd kiln
./install.sh            # or: make install
```

This builds a release `kiln` and installs:

| Path | Contents |
|---|---|
| `~/.kiln/bin/kiln` | the compiler and package tool |
| `~/.kiln/lib/kiln/std/` | the standard library sources |
| `~/.kiln/lib/kiln/runtime/` | the runtime libraries linked into every program |

`kiln` finds `lib/kiln` relative to its own executable, so the install can be moved anywhere and the clone deleted. `KILN_LIB_DIR` overrides the location. The installer adds `~/.kiln/bin` to `PATH` in `~/.profile`, plus `~/.bashrc`, `~/.zshrc` and fish when those shells are in use, and finishes by running `kiln doctor`. Open a new shell afterwards.

Options: `--prefix DIR` (or `KILN_PREFIX`; `make install PREFIX=DIR`), `--no-llvm`, `--no-modify-path`, and `--uninstall`. To upgrade, `git pull` and run the installer again.

## Check that it works

```sh
kiln version -v   # version, install paths, backends
kiln doctor       # checks the C toolchain and linker, then builds hello world
kiln new hello
cd hello
kiln run
# Hello, world!
```

`kiln new` creates `kiln.toml`, `src/main.kn`, `.gitignore` and a `README.md`. `kiln build`, `kiln run` and `kiln test` work from any directory inside the package.

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
| `kiln version [-v]` | Print the version (and install paths) |
| `kiln doctor` | Check the C toolchain, linker and install |

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
