# Kiln

Kiln is a compiled systems and backend programming language with **syntax as simple as Go and functionality like Rust**:

- Ownership and borrowing with no garbage collector.
- `Result` and `?` for error handling.
- Traits and generics.
- Goroutine-style tasks with `go`, channels using `<-`, and `select`.

> **Status:** early development. The compiler type-checks and borrow-checks programs (`kiln check`) and compiles them to native executables with a Cranelift backend (`kiln build`, `kiln run`): functions, structs, enums and `match`, generics, traits including `dyn`, closures, `Result` and `?`, `defer`, `Drop`, fixed-size arrays, and Go-style concurrency (`go` tasks on an M:N work-stealing scheduler, channels, `select`, scoped tasks) all work. The standard library is next. There are VS Code and Tree-sitter grammars. See [docs/ROADMAP.md](docs/ROADMAP.md).

```kiln
fn main() -> Result<(), io::Error> {
	listener := TcpListener::bind("0.0.0.0:8080")?
	for conn in listener.incoming() {
		conn := conn?
		go move {
			if let Err(e) = handle(conn) {
				eprintln("conn error: {e}")
			}
		}
	}
	return Ok(())
}
```

## Documentation

- [docs/SPEC.md](docs/SPEC.md): the language design spec (draft).
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the compiler is organized.
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones and current status.
- [website/](website/): the Kiln website and step-by-step tutorial. Build it with `mdbook build website`.
- [CONTRIBUTING.md](CONTRIBUTING.md): how to build, test, and contribute.

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
| `crates/kiln_borrowck` | Borrow checking: moves, NLL borrows, lifetimes, `Drop` order, `go`/`Send`/`Sync` |
| `crates/kiln_cli` | The `kiln` binary |
| `docs/` | Spec, architecture, roadmap |
| `examples/` | Sample `.kn` programs (parsed in tests) |

## License

Kiln is dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Kiln by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
