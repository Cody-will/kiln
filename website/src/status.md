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
| `Send`/`Sync` and `'static` checks for `go` and scoped tasks | ✅ Implemented (`kiln check`) |
| Code generation (Cranelift): functions, structs/enums/`match`, generics (monomorphized), traits and `dyn`, closures, `Result`/`?`, `defer`, `Drop`, `String`, `Vec`, `Box` | ✅ Implemented (`kiln build`, `kiln run`) |
| Fixed-size arrays `[T; N]` | ✅ Implemented |
| Integer overflow | ✅ Panics in debug builds, wraps with `--release` |
| Slice patterns, `\|` patterns with bindings | 🚧 Not yet in codegen (reported as errors) |
| Panics | ✅ Print a message and exit with status 101; 🚧 no unwinding (destructors and `defer` don't run on panic) |
| Concurrency: `go` and `Task<T>`, channels, `select`, `scope`/`spawn`, M:N work-stealing scheduler, deadlock detection | ✅ Implemented on Linux, macOS, and Windows |
| A panic inside a task | 🚧 Ends the whole program (exit 101); `join` never returns `Err` yet |
| Preemption | 🚧 Not yet: tasks switch at channel operations, `select`, and `join`, so a long loop without them keeps its thread busy |
| Standard library (`std::fs`, `std::net`, `std::time`, ...) | 🚧 Not yet: `std` imports resolve, but their items are opaque to the type checker |
| Multi-file packages, `kiln.toml`, registry | 🚧 Not yet: `kiln check` works on a single file (inline `mod` blocks are supported) |
| `kiln build`, `kiln run` | ✅ Implemented (single file) |
| `kiln fmt`, `kiln test`, language server | 🚧 Not yet |

## What `kiln check` catches today

- Type mismatches, with inference across a function body (`x := 5` is `i32` unless used as another integer type)
- Missing `return`s: Kiln never returns a function's last expression implicitly
- Unknown names, fields, and methods; wrong argument counts
- Trait impls that are missing methods or have mismatched signatures, and unsatisfied bounds such as `T: Ord`
- `?` used in a function that doesn't return `Result`/`Option`, or with an error type that has no `From` conversion
- Non-exhaustive `match`, looking inside nested patterns (enum variants, tuples, structs, `bool`)
- Assigning to, or taking `&mut` of, a variable not declared `mut`
- Sending on a receive-only channel, or receiving on a send-only one
- Using a value after it was moved (with a note showing where), including partial moves and moves in loops
- Conflicting borrows: `&mut` while a `&` is still in use, two `&mut` at once, assigning to or moving a borrowed value
- References that outlive what they point to, and returning references to locals
- Missing or mismatched lifetimes in signatures (`fn f(a: &str, b: &str) -> &str`)
- `go` tasks that borrow from the current function, or capture types that aren't `Send`
