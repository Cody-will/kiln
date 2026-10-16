# kiln_codegen

Native code generation for Kiln with [Cranelift](https://cranelift.dev).

- Monomorphizes MIR: each `(function, generic substitution)` pair used by the program becomes one native function, starting from `main`.
- Lays out types (`layout.rs`), translates MIR bodies (`func.rs`), and generates drop glue and `dyn Trait` vtables (`glue.rs`).
- Implements the prelude functions that have no Kiln body (`intrinsics.rs`): `String`, `str`, `Vec`, `Box`, float math, `panic`, `assert`, `drop`, `clone`.
- Starts tasks (`spawn`), and calls the runtime for `Task::join`, channels, `select`, and scopes.
- Checks integer overflow in debug builds (`Options { release: false }`); release builds wrap.
- Emits an object file and links it with the embedded `kiln_runtime` library (`link.rs`).

```rust
let out = std::path::Path::new("hello");
kiln_codegen::build_source("fun main() {\n\tprintln(\"hi\")\n}\n", out, kiln_codegen::Options::default())?;
```

Unsupported constructs (`std` items, slice patterns, ...) are reported as diagnostics at their source location.

See [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#kiln_codegen) for why Cranelift was chosen over LLVM, data layouts, and known gaps. End-to-end tests are in `crates/kiln_cli/tests/run.rs`.
