# Kiln Compiler Architecture

The compiler is a Cargo workspace written in Rust. This document describes what exists today and where each planned phase will live.

## Pipeline

```
source (.kn)
  │  kiln_lexer        tokens + doc comments + diagnostics
  ▼
tokens
  │  kiln_parser       AST (+ diagnostics, error recovery)
  ▼
AST
  │  kiln_resolve      module tree, imports, scopes, name resolution (+ prelude)
  ▼
resolved AST (path → Res, binding → LocalId side tables)
  │  kiln_typeck       type inference, traits/impls, `?`, explicit returns, mutability
  │  kiln_mir          MIR lowering: CFG with explicit moves, borrows, storage ends, defer, captures
  │  kiln_borrowck     moves/initialization, NLL borrow conflicts, lifetimes, go/Send/Sync
  │  kiln_codegen      MIR → Cranelift IR, monomorphization, drop glue, vtables, object file
  ▼
object file ── system linker + kiln_runtime (C: allocation, panics, printing, strings, Vec) ──► native executable
```

## Crates

### `kiln_lexer`

- `span.rs`:
  - `Span` is a pair of byte offsets.
  - `LineIndex` converts an offset to a line and column.
- `diagnostic.rs`: `Diagnostic` with rustc-style rendering. This is shared by all phases.
- `token.rs`: `TokenKind` and `Keyword`. `TokenKind::ends_statement` defines the semicolon-insertion rule.
- `lexer.rs`: a hand-written, single-pass lexer. Notable behavior:
  - **Semicolon insertion:** at a newline it inserts `Semi { implicit: true }` after a statement-ending token, unless the next line starts with `.`. A block comment that spans lines counts as a newline.
  - **String interpolation:**
    - The lexer splits a string into `StrPart::Text` and `StrPart::Interp { src, offset }`.
    - It does not lex the interpolated expression itself. The parser re-lexes `src` with `lex_at(src, offset)`, so spans still point into the original file.
    - Braces are written `{{` and `}}`.
  - **Lifetimes and chars:** `'a` is a lifetime and `'a'` is a char. The deciding factor is whether a closing quote follows the identifier characters.
  - **Number literals:** `1..2` and `5.secs()` are not floats, because a float needs a digit after the `.`.
  - **Error recovery:** errors are collected and lexing continues.

### `kiln_parser`

- **`ast.rs`:** the syntax tree.
  - Every node carries a `Span`.
  - A block is a flat list of statements. Whether a block yields a value (only `if`/`match` arms in value position, per SPEC §3.2) is decided later, not by the parser.
- **`parser.rs`:**
  - Items, statements, and patterns are parsed by recursive descent.
  - Expressions use Pratt-style binding powers: ranges < `||` < `&&` < comparisons < `|` < `^` < `&` < shifts < `+ -` < `* / %` < `as` < unary < postfix.
  - Key techniques:
    - **The `no_struct` flag:** inside `if`/`while`/`for`/`match`/`select` heads, `Path {` is not a struct literal. This is the same rule Rust uses.
    - **`>>` splitting:** `expect_gt` splits `>>`, `>=`, and `>>=` so that nested generics close correctly.
    - **Short-declaration lookahead:** `at_short_decl` peeks ahead for `x [mut] :=` and `(a, b mut) :=`.
    - **Newline-tolerant lists:** `comma_list` and `brace_list` ignore implicit semicolons between elements.
    - **Recovery:** `recover_item` skips to the next item keyword at depth 0. `recover_stmt` skips to the next statement boundary.
- **`lib.rs`:** the public API, `parse(src) -> Parse { file, errors }` and `parse_expr`.

### `kiln_resolve`

Name resolution over the AST, producing side tables keyed by span (no separate HIR yet).

- **`prelude.kn`:** the prelude written in Kiln (`Option`, `Result`, `String`, `Vec`, `Box`, `Range`, core traits, `println` and friends, and body-less method signatures the compiler provides). It is parsed with the normal parser; only the prelude may write inherent `impl`s on primitive and prelude types.
- **`std/*.kn`:** the standard library, written in Kiln (see SPEC §8.1). `lib.rs` (`STD_MODULES`, `builtin()`) concatenates the prelude and every module, as nested `pub mod std { pub mod io { ... } ... }` blocks, into one source parsed at the span offset `kiln_lexer::BUILTIN_BASE`, so builtin spans never collide with user spans and diagnostics inside std render with their own file names (`<std>/io.kn`). A body-less `fn kn_*` declaration in a std module is an extern call into the C runtime; other body-less functions are compiler intrinsics. The standard library is type-checked with every program; its bodies are borrow-checked by the `standard_library_borrow_checks` test (not on every user build).
- **`scope.rs`:** three phases. (1) `collect_items` creates a `Def` for every item and variant, building per-module type and value namespaces. (2) `bind_imports` resolves `import` trees; `std` paths resolve into the standard library, `core` paths become opaque `External` defs, and an imported name becomes a member of the importing module (re-export). (3) `walk_items` resolves every path in types, patterns, and bodies with a lexical scope stack: locals (declaration order is enforced, a `:=` initializer can't see its own name), generic params, `Self`/`self`, loop labels, or-pattern binding consistency, and type/value namespace mismatches with targeted help.
- **`lib.rs`:** `Resolved { defs, paths: Span → Res, bindings: Span → LocalId, locals, fns, impls, errors }`. `Res::Assoc { base, name }` defers `Type::item` lookups to the type checker.

### `kiln_typeck`

Type checking on top of `Resolved`.

- **`ty.rs`:** `Ty` and the `InferTable` (unification with occurs check, integer/float literal variables that default to `i32`/`f64`, snapshots for trial unification).
- **`collect.rs`:** lowers struct/enum fields, function and method signatures, impl headers, and traits; validates trait impls (missing/extra methods, signature mismatches), duplicate methods, and inherent impls on foreign types.
- **`check.rs`:** checks each body: bidirectional-lite inference, coercions (`!`, `&mut T → &T`, `&String → &str`, `&Vec<T>`/`&[T; N]` → `&[T]`, `&Box<T> → &T`, `&T → &dyn Trait`), method lookup with autoderef (inherent impls, trait impls and default methods, generic bounds, `dyn`), trait-bound obligations solved at the end of each function, Rust-style match ergonomics, match exhaustiveness for enums and `bool`, `?` with `From` conversion, channel operations, and mutability of assignment targets and `&mut` borrows. The explicit-return rule (SPEC §3.1) is enforced here: a non-unit function whose body can fall off the end is an error, with a dedicated message when the last statement is a bare expression.
- **`display.rs`:** renders types for diagnostics (`{integer}`, `<-chan T`, ...).

It also exports per-function results (`Typed`, `FnResults`: expression and local types, method resolutions with autoderef steps, callees) and trait queries (`is_copy`, `implements`, `has_drop_impl`) for later phases. `Send`/`Sync` are auto traits computed structurally over fields (with a cycle guard); explicit impls override.

Known gaps: associated types (`T::Item`), `impl Trait` (treated as `dyn`), array lengths, `Send`/`Sync` field analysis, and std (external) items are opaque (`{unknown}` types that unify with anything). Exhaustiveness checks only top-level variants.

### `kiln_mir`

Lowers each typed function body to a control-flow graph (`Body` of basic blocks). Places are a local plus field/deref/index projections; operands are `copy` (a non-consuming read) or `move`. Every `&`/`&mut` is an explicit `Ref` rvalue, and every variable's scope end is an explicit `StorageDead`, emitted in reverse declaration order. Lowering decisions that matter to the borrow checker:

- Calls record `flows`: which arguments the result may borrow from, derived from the callee's signature with lifetime elision (`sig.rs`).
- Method calls apply the type checker's autoderef steps as deref projections, then autoref the receiver *after* evaluating arguments (two-phase borrows). `&mut` arguments are reborrowed.
- Match ergonomics are reproduced: destructuring through `&T` adds a deref and binds by reference.
- `defer`red calls are lowered once into the shared exit block, before locals die.
- Closures and `go` blocks become separate bodies; captures are inferred from how the nested body uses each variable (moved → by value, assigned/`&mut` → `&mut`, otherwise `&`), or all by value for `move`.
- `go f(x)`, `go { }`, and `Scope::spawn` produce `Spawn` statements carrying the moved operands or capture list.

`kiln mir <file>` prints the MIR.

### `kiln_borrowck`

Checks each MIR body (see the module docs in `body.rs`):

1. **Moves and initialization** — a forward may-analysis of moved/uninitialized place paths. Reports use/borrow of moved values (with "value moved here" and loop notes), partial moves, uninitialized bindings, double assignment of immutable deferred-init variables, and moves out of references, indexes, or `Drop` types.
2. **Liveness** — backward analysis of which locals are read later. A local whose type implements `Drop` is used by its own `StorageDead`, which is how destructor order is enforced.
3. **Loans** — each `Ref` creates a loan; loans flow into the locals that hold them (copies, aggregates, call `flows`, reborrows through derefs). A loan is live while any holder is live (non-lexical lifetimes). Reads conflict with live mutable loans; writes, moves, and `&mut` conflict with any live loan of an overlapping place; `StorageDead` conflicts with live loans of that variable ("does not live long enough", with notes for where it was dropped and where the borrow is used later).
4. **Return place** — loans of locals reaching the return value are errors; incoming parameter loans must be among those the signature allows.
5. **Tasks** — `go` arguments and `go move` captures must not hold loans and must be `Send`; non-`move` `go` blocks may not borrow; scoped tasks require `Sync` for shared captures and `Send` otherwise.

`sigcheck.rs` checks signatures and type definitions: undeclared lifetime names and elided lifetimes that elision cannot resolve.

Loan sets are flow-sensitive: assigning a whole variable replaces the loans it holds. Closures that move a captured variable are callable once (calling twice is a use of a moved value). `defer` is modelled per execution: each `defer` pushes a call; the exit block pops and runs them last-in first-out (`DeferDispatch`), and a defer outside any loop can run at most once. Lifetime bounds (`'a: 'b` in generics and `where` clauses) extend which parameters a result may borrow from; `dyn Trait` without a lifetime is `'static`.

Closures passed to `Scope::spawn` are *pinned*: they stay live, with their loans, until the scope body returns, because the task can run until the scope joins it. This rejects two scoped tasks mutating one variable, and tasks borrowing locals of the scope body.

Known gaps: the checker treats `std` types as opaque; lifetime variance and higher-ranked lifetimes (`for<'a>`) are not modelled.

### `kiln_runtime`

A small C library (`runtime/kiln_rt.c`) compiled by `build.rs` with the `cc` crate. The static library's bytes are embedded in the compiler (`kiln_runtime::LIB`), so an installed `kiln` binary needs only a system linker. It provides allocation (`kn_alloc`/`kn_free`), panics (`kn_panic`: flush stdout, print `panic: ...` to stderr, exit 101), printing, `String` building and formatting of numbers, string comparison and the `str` methods, `Vec` growth, and the `defer` stack. `runtime/kiln_sched.c` is the concurrency runtime: an M:N work-stealing scheduler with assembly context switches (x86_64, AArch64) or fibers (Windows), guarded pooled task stacks, task handles, scopes, channels, `select`, and deadlock detection; it also owns the C `main`, which runs the program's `kn_main` as the first task. `runtime/kiln_std.c` backs the standard library: the error encoding (`-(kind << 32 | os_code)`) and OS error messages, clocks and a timer thread (sleeping tasks park), task-aware locks and atomics, the network poller (epoll, kqueue, WSAPoll) with non-blocking sockets, files, directories, environment, and processes. Loops call `kn_maybe_yield` every 1024 back-edges; it yields when the task has run 10 ms and other tasks are waiting. See `crates/kiln_runtime/README.md`.

Concurrency in codegen: a `go` lowers to a `Spawn` statement plus a nested MIR body (`GoBlock`, whose upvars the task owns); codegen moves the captured values into a heap environment, and `kn_go` runs the body's closure-ABI instance on a new task, returning the `Task<T>` handle. Channel ends and task handles are one pointer to a runtime object; their drop glue releases a reference. `select` lowers to one `kn_select` call over a case array, then a branch on the chosen index.

Data layout shared with codegen: `String` and `Vec<T>` are `{ ptr, cap, len }` (three words); `&str` and `&[T]` are `{ ptr, len }`; `Box<T>` is one pointer; `&dyn Trait` and `Box<dyn Trait>` are `{ data, vtable }`, where the vtable is `[drop_glue, size, align, methods...]` in trait declaration order.

### `kiln_codegen`

Lowers MIR to native code with [Cranelift](https://cranelift.dev).

**Why Cranelift instead of LLVM:** the inkwell/llvm-sys bindings need a system LLVM of one exact major version with `llvm-config` and development libraries. That is not installed on the development box, and on CI it means a different install per OS (apt on Linux, Homebrew on macOS, and on Windows no official package ships `llvm-config`, so it would need a custom LLVM build). Cranelift is pure Rust: it builds with `cargo` everywhere, keeps `cargo test` self-contained on all three CI platforms, and compiles quickly, which suits debug builds. The cost is weaker optimization than LLVM. MIR is the boundary, so an LLVM backend for release builds can be added later without touching the front end.

- `lib.rs`: the driver. A work queue of monomorphized instances: each `(function, body, substitution)` becomes one Cranelift function, declared on first use and defined when popped from the queue. It also declares the runtime functions, emits the C `main` wrapper (which prints `Error: <e>` and exits 1 when `main` returns `Err`), and `build_source` runs the whole pipeline down to a linked executable.
- `layout.rs`: sizes, alignments, and field offsets. Enums are a 4-byte tag followed by the largest payload. Every MIR local lives in a stack slot; values move with `memcpy`-style copies and Cranelift's optimizer promotes what it can.
- `func.rs`: MIR → Cranelift IR for one body: statements, terminators (including the `DeferDispatch` loop over the runtime defer stack), places with bounds-checked indexing, operands and constants, aggregates, coercions (unsizing to `dyn`, `&String` → `&str`, `&Vec<T>` → `&[T]`), calls (direct, through function pointers and closures, and virtual calls through vtables), operators, casts, and string interpolation. Drop flags track conditionally moved locals.
- `glue.rs`: drop glue per type (user `Drop` first, then fields), vtables, and inline clone code for `String`, `Vec`, `Box`, tuples, and ADTs.
- `intrinsics.rs`: the prelude functions that have no Kiln body (`String`, `str`, `Vec`, `Box`, float math, `panic`, `assert`, `drop`, `clone`) and the MIR intrinsics used by `for` loops.
- `link.rs`: writes the embedded runtime next to the object file and links with `cc` (or `$KILN_CC`/`$CC`) on Unix and `link.exe` (found via the `cc` crate's Visual Studio lookup) on Windows.

Constructs codegen does not support yet are reported as errors at their source location, never miscompiled: `std` library items, slice patterns, `|` patterns that bind variables, and non-literal `const`/`static` values.

Known codegen gaps: panics abort the process (no unwinding, so destructors don't run on panic); integer arithmetic wraps on overflow instead of panicking; no debug info; drops of the remaining fields after a partial move are not elaborated (those fields leak).

### `kiln_cli`

This builds the `kiln` binary: `lex`, `parse`, `check`, `mir`, `build`, and `run`. `kiln check` runs parsing, resolution, type checking, and borrow checking. `kiln build <file> [-o out]` adds code generation and linking; `kiln run <file> [args...]` builds into a temporary directory and runs the program, exiting with its status. `fmt` and `test` will be added as the matching phases land.

End-to-end tests live in `crates/kiln_cli/tests/run.rs`: each `tests/run/*.kn` program is built and run, and its stdout, stderr, and exit status are compared with the matching `.out` file (`KILN_BLESS=1` rewrites them). They run on Linux, macOS, and Windows in CI.

## Design principles for the codebase

- **No external dependencies in the front end.** This keeps builds fast and makes it easy to embed the front end in the language server.
- **Spans everywhere.** Every diagnostic points at a byte range.
- **Recover, don't stop.** Each phase reports as many errors as it reliably can.
- **One engine for the compiler and the IDE.** Later phases will move to a query-based design (salsa-style) so that `kiln-ls` reuses the compiler directly.

## Planned layout (from SPEC §11)

```
crates/
  kiln_lexer  kiln_parser  kiln_resolve  kiln_typeck  kiln_mir  kiln_borrowck
  kiln_codegen  kiln_runtime  kiln_fmt  kiln_pkg  kiln_ls  kiln_cli
crates/kiln_resolve/std/   the standard library, in Kiln
editors/tree-sitter-kiln/  editors/vscode-kiln/
```

The AST currently lives in `kiln_parser::ast`. It will move to its own crate if a lossless CST (needed by the formatter and language server) is introduced.
