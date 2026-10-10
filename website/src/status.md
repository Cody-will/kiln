# Implementation status

Kiln is specified in the [language specification](spec.md); the compiler is being built in milestones (see `docs/ROADMAP.md` in the repository).

| Area | Status |
|---|---|
| Lexer (with Go-style semicolon insertion) | ✅ Implemented |
| Parser (full spec grammar, error recovery) | ✅ Implemented |
| Name resolution (modules within a file, imports, scopes) | ✅ Implemented |
| Type checking: inference, structs/enums, traits/impls, generics, `?`, explicit returns, `:=`, mutability, channels | ✅ Implemented (`kiln check`) |
| VS Code (TextMate) and Tree-sitter grammars | ✅ Implemented |
| Lifetimes and the borrow checker | 🚧 Not yet: lifetimes are parsed but not checked, and moves are not tracked |
| Code generation (LLVM) | 🚧 Not yet: programs cannot run |
| Runtime: tasks, channels, `select`, scheduler | 🚧 Not yet: type-checked only |
| Standard library (`std::fs`, `std::net`, `std::time`, ...) | 🚧 Not yet: `std` imports resolve, but their items are opaque to the type checker |
| Multi-file packages, `kiln.toml`, registry | 🚧 Not yet: `kiln check` works on a single file (inline `mod` blocks are supported) |
| `kiln fmt`, `kiln build/run/test`, language server | 🚧 Not yet |

## What `kiln check` catches today

- Type mismatches, with inference across a function body (`x := 5` is `i32` unless used as another integer type)
- Missing `return`s: Kiln never returns a function's last expression implicitly
- Unknown names, fields, and methods; wrong argument counts
- Trait impls that are missing methods or have mismatched signatures, and unsatisfied bounds such as `T: Ord`
- `?` used in a function that doesn't return `Result`/`Option`, or with an error type that has no `From` conversion
- Non-exhaustive `match` on enums and `bool`
- Assigning to, or taking `&mut` of, a variable not declared `mut`
- Sending on a receive-only channel, or receiving on a send-only one
