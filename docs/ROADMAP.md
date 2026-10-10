# Kiln Roadmap

Status key: ✅ done · 🚧 in progress · ⬜ not started

| Milestone | Scope | Status |
|---|---|---|
| **M0: Front end** | Spec v0.1, lexer, parser with error recovery, `kiln lex` and `kiln parse`, CI | 🚧 lexer and parser done; TextMate and Tree-sitter grammars pending |
| **M1: First binaries** | Name resolution, type checking for fns/structs/enums/match, LLVM codegen via inkwell; hello world and fib run | ⬜ |
| **M2: Abstractions** | Traits, generics (monomorphization), `Result` and `?`, `Drop`, `defer` | ⬜ |
| **M3: Safety** | MIR, NLL borrow checker, lifetime elision, missing-`return` analysis | ⬜ |
| **M4: Concurrency** | Runtime: M:N scheduler, `go`, channels, `select`, scoped tasks, netpoller; `Send`/`Sync` checks | ⬜ |
| **M5: Std and packages** | io, fs, net, collections, sync, time; `kiln build`/`run`/`test`; `kiln.toml`, lockfile, registry client | ⬜ |
| **M6: Tooling** | `kiln fmt` (gofmt-style, tabs, trailing commas), `kiln-ls` MVP (diagnostics, hover, go-to-def, completion) | ⬜ |
| **M7: Backend-ready** | `std::http`, JSON, testing framework; dogfood a real service | ⬜ |
| **M8: 1.0 hardening** | Compiler fuzzing, conformance suite, performance, docs, stability guarantees | ⬜ |

## Next up (M0 → M1)

1. **Editor support:** a TextMate grammar and VS Code extension (`editors/vscode-kiln`), and a Tree-sitter grammar (`editors/tree-sitter-kiln`).
2. **Parser gaps** (see below).
3. **Lossless CST decision:** choose whether to build a lossless syntax tree now (in the rowan style), before the formatter and language server depend on the AST.
4. **Name resolution and HIR** (`kiln_resolve`).
5. **Type checker skeleton** (`kiln_types`) and the first LLVM output.

## Known parser gaps

- **`extern` blocks and FFI declarations:** not parsed yet (the `extern` keyword is reserved).
- **Lifetime-only where predicates** (`where 'a: 'b`): not supported yet; bounds on generic params do work.
- **Macros:** none (an open question in the spec). Built-ins like `println` are parsed as ordinary calls.
- **`scope |s| { ... }` sugar:** not special syntax. Use `scope(|s| { ... })` as an ordinary call.
- **Labeled blocks:** not supported. Only `loop`, `while`, and `for` take labels.
- **AST dump format:** `kiln parse` prints the AST with Rust's `Debug` format. A compact pretty-printer is planned.
