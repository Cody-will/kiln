# Self-hosted codegen: plan (WIP)

`kiln_codegen` (about 8k lines) drives Cranelift through Rust bindings, which
Kiln cannot call. The self-hosted backend will **emit textual LLVM IR** (`.ll`)
from the MIR produced by `compiler/src/lower.kn` and hand it to `clang`
(already used by `kiln_llvm`) to compile and link it against `kiln_runtime`.

Mapping from the Rust backend:
- `layout.rs` -> `compiler/src/layout.kn`: the same sizes, alignments and field
  offsets, so values stay ABI-compatible with the runtime.
- `func.rs` -> `compiler/src/emit.kn`: one `alloca` per MIR local, values moved
  through memory, and the same `fn(ret*, args*...)` convention with drop flags.
- `glue.rs` (drop, clone and eq glue) and `intrinsics.rs` -> emitted per
  monomorphized type.
- `inline.rs`, `bce.rs` and `promote.rs` are optimizations; leave them to
  LLVM's `-O` at first.
- The differential test compares program *output*, not IR: build each
  `examples/`/`tests/` program both ways and diff stdout and exit codes.

Bootstrap (`make bootstrap`): stage1 = kilnc built by the Rust `kiln`; stage2
= stage1 built by stage1; stage3 = stage2 built by stage2; require
`cmp stage2 stage3`.
