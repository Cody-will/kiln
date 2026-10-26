# Installation

## Quick install (Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/Cody-will/kiln/main/install.sh | sh
```

This downloads the prebuilt release for your machine (x86_64 or aarch64 Linux) from GitHub Releases, checks its SHA-256, installs it into `~/.kiln`, adds `~/.kiln/bin` to `PATH`, and runs `kiln doctor`. If there is no release for your machine, it clones the repository and builds from source instead (see below).

Options go after `sh -s --`, for example `curl -fsSL .../install.sh | sh -s -- --prefix /opt/kiln`:

| Option | |
|---|---|
| `--prefix DIR` | install into `DIR` (default `~/.kiln`; `KILN_PREFIX` also sets it) |
| `--version TAG` | install the release `TAG` instead of the latest |
| `--from-source` | build from source instead of downloading |
| `--no-modify-path` | leave shell startup files alone |
| `--uninstall` | remove the installation |

### Requirements

Kiln compiles programs through clang, so you need clang and a linker. lld is used when it is present, for faster debug links.

| Distribution | Command |
|---|---|
| Arch Linux | `sudo pacman -S --needed clang lld base-devel` |
| Debian, Ubuntu | `sudo apt install clang lld` |
| Fedora | `sudo dnf install clang lld` |

The prebuilt binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36, current Arch).

### Arch Linux (AUR)

`packaging/arch/PKGBUILD` packages the release as `kiln-bin` for the AUR. Until it is published there, build it from a clone:

```sh
cd packaging/arch && makepkg -si
```

It installs `/usr/bin/kiln` and `/usr/lib/kiln`, and puts the nvim plugin on the system package path.

## Build from source

Source builds also need [Rust](https://rustup.rs) 1.82 or newer and `git`. LLVM 18 is optional: with it, the Rust-hosted compiler's `--compiler=rust --release` builds use LLVM (`apt install llvm-18-dev`).

```sh
git clone https://github.com/Cody-will/kiln.git
cd kiln
./install.sh            # or: make install
```

Run from a clone, `install.sh` builds that clone: a release `kiln`, and then kilnc, the self-hosted compiler, bootstrapped with itself. Source-only options: `--no-llvm` and `--no-kilnc`. To upgrade, `git pull` and run the installer again.

`make dist` builds the release tarball for the current machine (`target/dist/kiln-<arch>-linux.tar.gz`, from `tools/release.sh`). Tagging `v*` builds both architectures in CI and attaches them to the GitHub release.

## Layout

| Path | Contents |
|---|---|
| `~/.kiln/bin/kiln` | the toolchain: build, run, test, fmt, the language server |
| `~/.kiln/lib/kiln/kilnc` | the self-hosted compiler, the default code generator |
| `~/.kiln/lib/kiln/std/` | the standard library sources |
| `~/.kiln/lib/kiln/runtime/` | the runtime libraries linked into every program |
| `~/.kiln/lib/kiln/editors/` | the VS Code extension and the nvim plugin ([Editors](editors.md)) |

`kiln` finds `lib/kiln` relative to its own executable, so the install can be moved anywhere. `KILN_LIB_DIR` overrides the location. The installer adds `~/.kiln/bin` to `PATH` in `~/.profile`, plus `~/.bashrc`, `~/.zshrc` and fish when those shells are in use. Open a new shell afterwards.

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
| `kiln lsp` | Run the language server (editors start it) |
| `kiln editor install <vscode\|nvim>` | Install editor support ([Editors](editors.md)) |

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
