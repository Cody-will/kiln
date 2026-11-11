# Implementation status

Kiln is specified in the [language specification](spec.md); the compiler is being built in milestones (see `docs/ROADMAP.md` in the repository).

| Area | Status |
|---|---|
| Lexer (with Go-style semicolon insertion) | ✅ Implemented |
| Parser (full spec grammar, error recovery) | ✅ Implemented |
| Name resolution (modules within a file, imports, scopes) | ✅ Implemented |
| Type checking: inference, structs/enums, traits/impls, generics, `?`, explicit returns, `:=`, mutability, channels | ✅ Implemented (`kiln check`) |
| VS Code (TextMate) and Tree-sitter grammars | ✅ Implemented |
| Ownership and borrow checking: moves, borrows (non-lexical lifetimes), lifetimes and elision, `Drop` order, `defer`, closure captures | ✅ Implemented (`kiln check`) |
| `Send`/`Sync` and `'static` checks for `spawn` and scoped tasks | ✅ Implemented (`kiln check`) |
| Code generation (Cranelift): functions, structs/enums/`match`, generics (monomorphized), traits and `dyn`, closures, `result`/`?`, `defer`, `Drop`, `string`, `Vec`, `Box` | ✅ Implemented (`kiln build`, `kiln run`; optional LLVM backend for `--release`) |
| Fixed-size arrays `[T; N]` | ✅ Implemented |
| `impl Trait` return types | ✅ Monomorphized (concrete type inferred from the body, no boxing); not in trait methods or recursive functions |
| Associated types | ✅ `type Item` in traits/impls, `Self::Item` / `T::Item`, `Iterator<Item = X>` bounds; positional `Iterator<T>` still accepted |
| Patterns | ✅ Nested enum/struct/tuple patterns, slice patterns (`[a, .., b]` on arrays, slices, `Vec`), `|` patterns with bindings; exhaustiveness checks all of them |
| Debug info | ✅ DWARF line tables in debug builds on Linux (`gdb`, `addr2line`, `perf` show `file.kn:line`); 🚧 no variable info, none for LLVM release builds yet |
| Integer overflow | ✅ Panics in debug builds, wraps with `--release` |
| Panics | ✅ Pending `defer` calls run (innermost first, as in Go), then the message is printed and the process exits with status 101; `std::panic::recover` stops a panic and returns `Err(message)`. Destructors don't run on panic (no unwinding; locals leak). `defer` on panic and `recover` work on Linux, macOS, and Windows x64 |
| Concurrency: `spawn` and `Task<T>`, channels, `select`, `scope`/`spawn`, M:N work-stealing scheduler, deadlock detection | ✅ Implemented on Linux, macOS, and Windows |
| A panic inside a task | ✅ `join` returns `Err(Panic)` with the message (Linux, macOS, Windows x64); a detached task's panic ends the program |
| Preemption | ✅ Loops yield after a task has run 10 ms while others wait; tasks also switch at channels, `select`, `join`, locks, timers, and I/O |
| Standard library phase 1: `time`, `sync`, `io`, `fs`, `path`, `os`, `net` (TCP/UDP on the netpoller), `net::http` (server and client), `log`, `fmt`, `mem`, `ptr` | ✅ Implemented on Linux, macOS, and Windows; written in Kiln on the C runtime ([reference](stdlib.md)) |
| Standard library phase 2: `strings`, `strconv`, `bytes`, `collections`, `math`, `rand`, `errors`, `flag`, `encoding` (JSON with `@derive(Json)`, base64, hex, CSV), `crypto`, `regexp`, `context`, net timeouts | ✅ Implemented |
| HTTPS (TLS) | ✅ `std::net::tls` and `https://` in `std::net::http` (rustls) |
| `static` items | ✅ Implemented |
| Multi-file packages, `kiln.toml` | ✅ `mod name` files, `kiln new`, path dependencies, build caching |
| FFI: `extern "C"`, `@repr(C)`, `[link]` in `kiln.toml` | ✅ C functions (including variadic), C function pointers and callbacks, `@export extern "C" fun`, C-layout structs, system libraries and C sources (C sources not on Windows yet) |
| Package registry, lockfile | ✅ git/file index, `kiln add`/`update`/`publish`, semver, sha256 `kiln.lock`, offline cache; default index is [Cody-will/kiln-index](https://github.com/Cody-will/kiln-index) (publish via PR) |
| `kiln build`, `kiln run` | ✅ Single files and multi-file packages (`kiln.toml`, `mod`, path and registry deps) |
| `kiln test` (`@test`, `@should_panic`, `@ignore`, process-per-test, parallel) | ✅ Implemented (single files and directories) |
| `kiln fmt` | ✅ canonical formatter with `--check` and `--stdin` |
| Language server | ✅ `kiln lsp`: kilnc-backed diagnostics and navigation, hover, definition, references, rename, completion, symbols, formatting, inlay hints, semantic tokens, quick fixes; wired into the VS Code extension |

## What `kiln check` catches today

- Type mismatches, with inference across a function body (`x := 5` is `i32` unless used as another integer type)
- Missing `return`s: Kiln never returns a function's last expression implicitly
- Unknown names, fields, and methods; wrong argument counts
- Trait impls that are missing methods or have mismatched signatures, and unsatisfied bounds such as `T: Ord`
- `?` used in a function that doesn't return `result`/`option`, or with an error type that has no `From` conversion
- Non-exhaustive `match`, looking inside nested patterns (enum variants, tuples, structs, `bool`, integer literals and ranges), naming a missing case such as `Some(false)` or `i32::MIN..=-1`
- Assigning to, or taking `&mut` of, a variable not declared `mut`
- Sending on a receive-only channel, or receiving on a send-only one
- Using a value after it was moved (with a note showing where), including partial moves and moves in loops
- Conflicting borrows: `&mut` while a `&` is still in use, two `&mut` at once, assigning to or moving a borrowed value
- References that outlive what they point to, and returning references to locals
- Missing or mismatched lifetimes in signatures (`fun f(a &str, b &str) &str`)
- `spawn` tasks that borrow from the current function, or capture types that aren't `Send`
