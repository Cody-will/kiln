# Kiln Roadmap

Status key: ✅ done · 🚧 in progress · ⬜ not started

| Milestone | Scope | Status |
|---|---|---|
| **M0: Front end** | Spec v0.1, lexer, parser with error recovery, `kiln lex` and `kiln parse`, CI | ✅ lexer, parser, VS Code TextMate grammar, Tree-sitter grammar |
| **M1: First binaries** | Name resolution, type checking for fns/structs/enums/match, native codegen; hello world and fib run | ✅ `kiln_resolve`, `kiln_typeck`, and a Cranelift backend (`kiln_codegen`, `kiln_runtime`); `kiln build` and `kiln run` |
| **M2: Abstractions** | Traits, generics (monomorphization), `Result` and `?`, `Drop`, `defer` | ✅ all compile and run, including `dyn Trait` through vtables and closures |
| **M3: Safety** | MIR, NLL borrow checker, lifetime elision, (missing-`return` is already checked in `kiln_typeck`) | ✅ `kiln_mir` and `kiln_borrowck` (moves, NLL borrows, lifetimes, `Drop` order, `defer`, closures, `go`/`Send`/`Sync`) in `kiln check`; see known gaps in ARCHITECTURE |
| **M4: Concurrency** | Runtime: M:N scheduler, `go`, channels, `select`, scoped tasks, netpoller; `Send`/`Sync` checks | 🚧 static checks done (`'static` + `Send` for `go`, `Sync` for scoped tasks); runtime pending |
| **M5: Std and packages** | io, fs, net, collections, sync, time; `kiln build`/`run`/`test`; `kiln.toml`, lockfile, registry client | ⬜ |
| **M6: Tooling** | `kiln fmt` (gofmt-style, tabs, trailing commas), `kiln-ls` MVP (diagnostics, hover, go-to-def, completion) | ⬜ |
| **M7: Backend-ready** | `std::http`, JSON, testing framework; dogfood a real service | ⬜ |
| **M8: 1.0 hardening** | Compiler fuzzing, conformance suite, performance, docs, stability guarantees | ⬜ |

## Next up

1. **Concurrency runtime (M4):** an M:N scheduler in `kiln_runtime`, `go`, channels with `<-`, `select`, scoped tasks; codegen for each.
2. **Type checker gaps:** array lengths (which unblocks fixed-size arrays in codegen), associated types, `impl Trait` as a distinct type, deeper exhaustiveness (nested patterns, integer ranges), method resolution on integer literals before defaulting, and typed signatures for `std` instead of opaque externals.
3. **Codegen gaps:** unwinding (or at least running `defer` on panic), overflow-checked arithmetic, debug info, slice patterns, `|` patterns with bindings, per-field drops after partial moves, an optimizing LLVM backend for release builds.
4. **Lossless CST decision:** choose whether to build a lossless syntax tree (in the rowan style) before the formatter and language server depend on the AST.
5. **Borrow checker refinements:** per-field drop elaboration after partial moves, lifetime variance, higher-ranked lifetimes.

## Known parser gaps

- **`extern` blocks and FFI declarations:** not parsed yet (the `extern` keyword is reserved).
- **Macros:** none (an open question in the spec). Built-ins like `println` are parsed as ordinary calls.
- **`scope |s| { ... }` sugar:** not special syntax. Use `scope(|s| { ... })` as an ordinary call.
- **Labeled blocks:** not supported. Only `loop`, `while`, and `for` take labels.
- **AST dump format:** `kiln parse` prints the AST with Rust's `Debug` format. A compact pretty-printer is planned.
