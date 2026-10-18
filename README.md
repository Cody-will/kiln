# Kiln

Kiln is a compiled systems and backend programming language with **syntax as simple as Go and functionality like Rust**:

- Ownership and borrowing with no garbage collector.
- `result` and `?` for error handling.
- Traits and generics.
- Goroutine-style tasks with `spawn`, channels using `<-`, and `select`.

> **Status:** early development. The compiler type-checks and borrow-checks programs (`kiln check`) and compiles them to native executables with a Cranelift backend (`kiln build`, `kiln run`) and an optional LLVM backend for `--release` (`--backend=llvm`, see [docs/LLVM.md](docs/LLVM.md)). The default code generator is **kilnc**, the self-hosted compiler, which `install.sh` bootstraps; `--compiler=rust` selects the built-in one (see [docs/SELF_HOSTING.md](docs/SELF_HOSTING.md)): functions, structs, enums and `match`, generics, traits including `dyn`, closures, `result` and `?`, `defer`, `Drop`, fixed-size arrays, and Go-style concurrency (`spawn` tasks on an M:N work-stealing scheduler, channels, `select`, scoped tasks) all work. The standard library is next. There are VS Code and Tree-sitter grammars. See [docs/ROADMAP.md](docs/ROADMAP.md).

```kiln
fun main() result<void, io::Error> {
	listener := TcpListener::bind("0.0.0.0:8080")?
	for conn in listener.incoming() {
		conn := conn?
		spawn move {
			if let Err(e) = handle(conn) {
				eprintln("conn error: {e}")
			}
		}
	}
	return Ok()
}
```

## Documentation

- [docs/SPEC.md](docs/SPEC.md): the language design spec (draft).
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the compiler is organized.
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones and current status.
- [website/](website/): the Kiln website and step-by-step tutorial. Build it with `mdbook build website`.
- [CONTRIBUTING.md](CONTRIBUTING.md): how to build, test, and contribute.

## Install

On Linux (x86_64 or aarch64), with clang installed (Arch: `pacman -S --needed clang lld base-devel`):

```sh
curl -fsSL https://raw.githubusercontent.com/Cody-will/kiln/main/install.sh | sh
kiln new hello && cd hello && kiln run
```

The installer downloads a prebuilt release into `~/.kiln`, adds it to `PATH`, and runs `kiln doctor`. From a clone, `./install.sh` (or `make install`) builds from source instead. See [website/src/install.md](website/src/install.md) for options, the Arch package, and building from source. Note: release downloads need the repository to be public.

Editor support: `kiln editor install vscode` or `kiln editor install nvim` (see [website/src/editors.md](website/src/editors.md)).

## Building

Requires Rust 1.80 or later.

```sh
cargo build
cargo test
./target/debug/kiln lex examples/hello.kn     # dump tokens
./target/debug/kiln parse examples/server.kn  # dump the AST
./target/debug/kiln check examples/shapes.kn  # resolve names and type-check
```

Example programs are in [`examples/`](examples/). Kiln source files use the `.kn` extension.

## Repository layout

| Path | Contents |
|---|---|
| `crates/kiln_lexer` | Tokens, lexer (with Go-style semicolon insertion), spans, diagnostics |
| `crates/kiln_parser` | AST and recursive-descent/Pratt parser |
| `crates/kiln_resolve` | Name resolution: modules, imports, scopes, prelude |
| `website/` | The website and tutorial book (mdBook); see `website/README.md` |
| `crates/kiln_typeck` | Type checking: inference, traits/impls, generics, `?`, explicit returns |
| `crates/kiln_mir` | Mid-level IR: control-flow graph lowered from the typed AST (`kiln mir`) |
| `crates/kiln_borrowck` | Borrow checking: moves, NLL borrows, lifetimes, `Drop` order, `spawn`/`Send`/`Sync` |
| `crates/kiln_cli` | The `kiln` binary |
| `docs/` | Spec, architecture, roadmap |
| `examples/` | Sample `.kn` programs (parsed in tests) |

## License

Kiln is dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Kiln by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
