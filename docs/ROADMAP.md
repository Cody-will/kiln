# Kiln Roadmap

Status key: ✅ done · 🚧 in progress · ⬜ not started

| Milestone | Scope | Status |
|---|---|---|
| **M0: Front end** | Spec v0.1, lexer, parser with error recovery, `kiln lex` and `kiln parse`, CI | ✅ lexer, parser, VS Code TextMate grammar, Tree-sitter grammar |
| **M1: First binaries** | Name resolution, type checking for fns/structs/enums/match, native codegen; hello world and fib run | ✅ `kiln_resolve`, `kiln_typeck`, and a Cranelift backend (`kiln_codegen`, `kiln_runtime`); `kiln build` and `kiln run` |
| **M2: Abstractions** | Traits, generics (monomorphization), `Result` and `?`, `Drop`, `defer` | ✅ all compile and run, including `dyn Trait` through vtables and closures |
| **M3: Safety** | MIR, NLL borrow checker, lifetime elision, (missing-`return` is already checked in `kiln_typeck`) | ✅ `kiln_mir` and `kiln_borrowck` (moves, NLL borrows, lifetimes, `Drop` order, `defer`, closures, `spawn`/`Send`/`Sync`) in `kiln check`; see known gaps in ARCHITECTURE |
| **M4: Concurrency** | Runtime: M:N scheduler, `spawn`, channels, `select`, scoped tasks, netpoller; `Send`/`Sync` checks | ✅ work-stealing scheduler, `spawn` → `Task<T>`, channels, `select`, scoped tasks, deadlock detection, `Send`/`Sync` enforced; netpoller and timers move to M5 with `std::net`/`std::time` |
| **M5: Std and packages** | io, fs, net, collections, sync, time; `kiln build`/`run`/`test`; `kiln.toml`, lockfile, registry client | ✅ packages done: `kiln.toml`, `src/` layout, `mod` files, path and registry dependencies (`kiln add/update/publish`, semver, sha256 `kiln.lock`, offline cache), `kiln new`, cached builds; default public index at Cody-will/kiln-index. std phases 1–3 done (see Next up) |
| **M6: Tooling** | `kiln fmt` (gofmt-style, tabs, trailing commas), `kiln-ls` MVP (diagnostics, hover, go-to-def, completion) | ✅ `kiln fmt` (token-based, self-verifying, idempotence-tested over the repo) and `kiln lsp` (kilnc-backed diagnostics and navigation, hover, definition, references, rename, completion, symbols, formatting, inlay hints, semantic tokens, quick fixes; VS Code client). Next: multi-file workspaces and incremental analysis |
| **M7: Backend-ready** | `std::net::http`, JSON, testing framework; dogfood a real service | ✅ `http`/`http2`/`json`/`kiln test` done; `examples/todo_api` dogfooded in CI with live PostgreSQL |
| **M8: 1.0 hardening** | Compiler fuzzing, conformance suite, performance, docs, stability guarantees | ⬜ |
| **Self-hosting** | Rewrite the compiler in Kiln; kilnc daily driver; retire Rust codegen (docs/SELF_HOSTING.md) | 🚧 **kilnc is the compiler for every normal workflow** (`build`/`run` incl. `[link]` and `--backend=llvm`, `check`, `test`, LSP diagnostics and navigation); Rust crates remain only as bootstrap seed and differential-test reference (`--compiler=rust`). Next: move Rust crates under `bootstrap/reference/`, then delete unused Rust codegen |

## Next up

1. **Standard library phase 2:** ✅ done (strings through `context`, `kiln test`, net timeouts). Since done: HTTP client/server timeouts, duration parsing and flags, website sections for every phase 2 module, HTTPS via rustls (`std::net::tls`), mTLS and OS trust stores. Backend libraries: HTTP/2 (`std::net::http2`, ALPN), WebSockets (`std::net::websocket`) and a PostgreSQL client with a pool (`std::db::postgres`). Leftovers: DNS lookups aren't cancellable beyond connect timeouts, Unicode case folding in `regexp`.
2. **Standard library phase 3:** ✅ done (`std::compress`, `std::mime`, `std::url`, `std::template`).
3. **Runtime refinements:** growable task stacks, IOCP on Windows (stack overflow is now detected and reported; frames over 4 MiB are rejected at compile time). Codegen still copies large array temporaries (a 1 MB local array uses ~4 MB of frame).
4. **Type checker gaps:** associated-type projections in impl headers and expression position (associated types and return-position `impl Trait` are done), method resolution on integer literals before defaulting.
5. **Codegen gaps:** destructors on panic (locals leak; `recover` and `defer` on panic are done on Linux, macOS, and Windows x64), and debug info beyond line tables (variables, types) and for LLVM builds. Done: `static` items, the optimizing LLVM backend for release builds (`crates/kiln_llvm`, docs/LLVM.md), slice patterns, `|` patterns with bindings, `defer` on panic / `recover` (Linux, macOS, Windows x64), and DWARF line tables in Cranelift debug builds (Linux).
6. **Lossless CST decision:** choose whether to build a lossless syntax tree (in the rowan style) before the formatter and language server depend on the AST.
7. **Borrow checker refinements:** lifetime variance, higher-ranked lifetimes; reading a `&'t` field through `&self` should keep lifetime `'t` (today it borrows `self`).
8. **Self-hosting:** kilnc is the daily driver (see docs/SELF_HOSTING.md). `kiln check`, `kiln test`, native `[link]`, and LSP diagnostics and navigation run on kilnc. Remaining: seed-only bootstrap docs in release notes, then relocate Rust crates under `bootstrap/reference/` and delete unused codegen.

## Known parser gaps

- **Macros:** none (an open question in the spec). Built-ins like `println` are parsed as ordinary calls.
- **`scope |s| { ... }` sugar:** not special syntax. Use `scope(|s| { ... })` as an ordinary call.
- **Labeled blocks:** not supported. Only `loop`, `while`, and `for` take labels.
- **AST dump format:** `kiln parse` prints the AST with Rust's `Debug` format. A compact pretty-printer is planned.

- Package registry: git/file index, `kiln add/update/publish`, semver, checksummed lockfile, offline cache (docs/REGISTRY.md). Default public index: [Cody-will/kiln-index](https://github.com/Cody-will/kiln-index) (empty packages so far; publish via PR).
