# kiln_codegen

Native code generation for Kiln with [Cranelift](https://cranelift.dev).

- Monomorphizes MIR: each `(function, generic substitution)` pair used by the program becomes one native function, starting from `main`.
- Lays out types (`layout.rs`), translates MIR bodies (`func.rs`), and generates drop glue and `dyn Trait` vtables (`glue.rs`).
- Implements the prelude functions that have no Kiln body (`intrinsics.rs`): `String`, `str`, `Vec`, `Box`, float math, `panic`, `assert`, `drop`, `clone`.
- Emits an object file and links it with the embedded `kiln_runtime` library (`link.rs`).

```rust
let out = std::path::Path::new("hello");
kiln_codegen::build_source("fn main() {\n\tprintln(\"hi\")\n}\n", out)?;
```

Unsupported constructs (`go`, channels, `select`, fixed-size arrays, `std` items, ...) are reported as diagnostics at their source location.

See [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md#kiln_codegen) for why Cranelift was chosen over LLVM, data layouts, and known gaps. End-to-end tests are in `crates/kiln_cli/tests/run.rs`.
