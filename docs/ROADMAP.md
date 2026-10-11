# Kiln Roadmap

Status key: ✅ done · 🚧 in progress · ⬜ not started

| Milestone | Scope | Status |
|---|---|---|
| **M0: Front end** | Spec v0.1, lexer, parser with error recovery, `kiln lex` and `kiln parse`, CI | ✅ lexer, parser, VS Code TextMate grammar, Tree-sitter grammar |
| **M1: First binaries** | Name resolution, type checking for fns/structs/enums/match, native codegen; hello world and fib run | ✅ `kiln_resolve`, `kiln_typeck`, and a Cranelift backend (`kiln_codegen`, `kiln_runtime`); `kiln build` and `kiln run` |
| **M2: Abstractions** | Traits, generics (monomorphization), `Result` and `?`, `Drop`, `defer` | ✅ all compile and run, including `dyn Trait` through vtables and closures |
| **M3: Safety** | MIR, NLL borrow checker, lifetime elision, (missing-`return` is already checked in `kiln_typeck`) | ✅ `kiln_mir` and `kiln_borrowck` (moves, NLL borrows, lifetimes, `Drop` order, `defer`, closures, `go`/`Send`/`Sync`) in `kiln check`; see known gaps in ARCHITECTURE |
| **M4: Concurrency** | Runtime: M:N scheduler, `go`, channels, `select`, scoped tasks, netpoller; `Send`/`Sync` checks | ✅ work-stealing scheduler, `go` → `Task<T>`, channels, `select`, scoped tasks, deadlock detection, `Send`/`Sync` enforced; netpoller and timers move to M5 with `std::net`/`std::time` |
| **M5: Std and packages** | io, fs, net, collections, sync, time; `kiln build`/`run`/`test`; `kiln.toml`, lockfile, registry client | 🚧 packages done: `kiln.toml`, `src/` layout, `mod` files, path dependencies, `kiln new`, cached builds; registry and lockfile next. std phase 1 done: `time`, `sync`, `io`, `fs`, `path`, `os`, `net`, `net::http`, `log` (written in Kiln on the C runtime), preemption at loop back-edges; phase 2 next |
| **M6: Tooling** | `kiln fmt` (gofmt-style, tabs, trailing commas), `kiln-ls` MVP (diagnostics, hover, go-to-def, completion) | ✅ `kiln fmt` (token-based, self-verifying, idempotence-tested over the repo) and `kiln lsp` (single-file: diagnostics, hover, definition, references, rename, completion, symbols, formatting, inlay hints; VS Code client). Next: multi-file workspaces, semantic tokens, code actions, incremental analysis |
| **M7: Backend-ready** | `std::net::http`, JSON, testing framework; dogfood a real service | ⬜ |
| **M8: 1.0 hardening** | Compiler fuzzing, conformance suite, performance, docs, stability guarantees | ⬜ |

## Next up

1. **Standard library phase 2:** ✅ done (strings through `context`, `kiln test`, net timeouts). Since done: HTTP client/server timeouts, duration parsing and flags, website sections for every phase 2 module, HTTPS via rustls (`std::net::tls`). Leftovers: DNS lookups aren't cancellable beyond connect timeouts, Unicode case folding in `regexp`, TLS client certificates (mTLS) and OS trust stores.
2. **Standard library phase 3:** `compress`, `mime`, `url`, templates.
3. **Runtime refinements:** growable task stacks, per-task panics that `join` reports as `Err(Panic)` instead of ending the program, IOCP on Windows (stack overflow is now detected and reported; frames over 4 MiB are rejected at compile time). Codegen still copies large array temporaries (a 1 MB local array uses ~4 MB of frame).
4. **Type checker gaps:** associated types, `impl Trait` as a distinct type, deeper exhaustiveness (nested patterns, integer ranges), method resolution on integer literals before defaulting.
5. **Codegen gaps:** `static` items, unwinding (or at least running `defer` on panic), debug info, slice patterns, `|` patterns with bindings, an optimizing LLVM backend for release builds.
6. **Lossless CST decision:** choose whether to build a lossless syntax tree (in the rowan style) before the formatter and language server depend on the AST.
7. **Borrow checker refinements:** lifetime variance, higher-ranked lifetimes; reading a `&'t` field through `&self` should keep lifetime `'t` (today it borrows `self`).
8. **Parser:** `Vec<chan<- T>>` fails to parse (the `>>` after a channel type); use a `type` alias for now.

## Known parser gaps

- **`extern` blocks and FFI declarations:** not parsed yet (the `extern` keyword is reserved).
- **Macros:** none (an open question in the spec). Built-ins like `println` are parsed as ordinary calls.
- **`scope |s| { ... }` sugar:** not special syntax. Use `scope(|s| { ... })` as an ordinary call.
- **Labeled blocks:** not supported. Only `loop`, `while`, and `for` take labels.
- **AST dump format:** `kiln parse` prints the AST with Rust's `Debug` format. A compact pretty-printer is planned.
