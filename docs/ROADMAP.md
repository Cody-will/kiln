# Kiln Roadmap

Status key: ✅ done · 🚧 in progress · ⬜ not started

| Milestone | Scope | Status |
|---|---|---|
| **M0: Front end** | Spec v0.1, lexer, parser with error recovery, `kiln lex` and `kiln parse`, CI | ✅ lexer, parser, VS Code TextMate grammar, Tree-sitter grammar |
| **M1: First binaries** | Name resolution, type checking for fns/structs/enums/match, LLVM codegen via inkwell; hello world and fib run | 🚧 `kiln_resolve` and `kiln_typeck` done (`kiln check`); codegen next |
| **M2: Abstractions** | Traits, generics (monomorphization), `Result` and `?`, `Drop`, `defer` | 🚧 type checking of traits, generics, and `?` done; monomorphization and `Drop` pending |
| **M3: Safety** | MIR, NLL borrow checker, lifetime elision, (missing-`return` is already checked in `kiln_typeck`) | ✅ `kiln_mir` and `kiln_borrowck` (moves, NLL borrows, lifetimes, `Drop` order, `defer`, closures, `go`/`Send`/`Sync`) in `kiln check`; see known gaps in ARCHITECTURE |
| **M4: Concurrency** | Runtime: M:N scheduler, `go`, channels, `select`, scoped tasks, netpoller; `Send`/`Sync` checks | 🚧 static checks done (`'static` + `Send` for `go`, `Sync` for scoped tasks); runtime pending |
| **M5: Std and packages** | io, fs, net, collections, sync, time; `kiln build`/`run`/`test`; `kiln.toml`, lockfile, registry client | ⬜ |
| **M6: Tooling** | `kiln fmt` (gofmt-style, tabs, trailing commas), `kiln-ls` MVP (diagnostics, hover, go-to-def, completion) | ⬜ |
| **M7: Backend-ready** | `std::http`, JSON, testing framework; dogfood a real service | ⬜ |
| **M8: 1.0 hardening** | Compiler fuzzing, conformance suite, performance, docs, stability guarantees | ⬜ |

## Next up (M1)

1. **Type checker gaps:** associated types, `impl Trait` as a distinct type, array lengths, deeper exhaustiveness (nested patterns, integer ranges), method resolution on integer literals before defaulting, and typed signatures for `std` instead of opaque externals.
2. **Lossless CST decision:** choose whether to build a lossless syntax tree (in the rowan style) before the formatter and language server depend on the AST.
3. **Codegen:** lower MIR (already built for the borrow checker) to LLVM via inkwell; run hello world and fib. MIR will need explicit drop elaboration and monomorphization first.
4. **Borrow checker refinements:** flow-sensitive loan sets, lifetime bounds (`'a: 'b`), checking moves out of by-reference closure captures, `unsafe impl Send`.

## Known parser gaps

- **`extern` blocks and FFI declarations:** not parsed yet (the `extern` keyword is reserved).
- **Lifetime-only where predicates** (`where 'a: 'b`): not supported yet; bounds on generic params do work.
- **Macros:** none (an open question in the spec). Built-ins like `println` are parsed as ordinary calls.
- **`scope |s| { ... }` sugar:** not special syntax. Use `scope(|s| { ... })` as an ordinary call.
- **Labeled blocks:** not supported. Only `loop`, `while`, and `for` take labels.
- **AST dump format:** `kiln parse` prints the AST with Rust's `Debug` format. A compact pretty-printer is planned.
