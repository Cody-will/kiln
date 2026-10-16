# The LLVM backend

Kiln has two code generators:

| | Cranelift | LLVM |
|---|---|---|
| Used for | debug builds (always), release builds without LLVM | `--release` when `kiln` was built with the `llvm` feature |
| Compile speed | fast | slower (full `O3` pipeline) |
| Code speed | good | close to Rust (see `bench/RESULTS.md`) |
| Needs | nothing | LLVM 18 at build time of `kiln` |

`kiln build --release` (and `run`, `test`) picks LLVM automatically when it
is available. `--backend=cranelift` or `--backend=llvm` overrides the
choice. A `kiln` built without the feature works exactly as before, and
`--backend=llvm` gives an error that says how to get it.

## Building `kiln` with LLVM

The backend uses [inkwell](https://github.com/TheDan64/inkwell) 0.6 with
LLVM **18**.

Debian / Ubuntu:

```sh
sudo apt-get install llvm-18-dev libpolly-18-dev zlib1g-dev libzstd-dev
export LLVM_SYS_181_PREFIX=/usr/lib/llvm-18
cargo build --release -p kiln_cli --features llvm           # static libLLVM
cargo build --release -p kiln_cli --features llvm-dynamic   # links libLLVM-18.so (faster to build)
```

macOS (Homebrew): `brew install llvm@18`, then
`export LLVM_SYS_181_PREFIX=$(brew --prefix llvm@18)`.

Other systems: install LLVM 18 with its development files (`llvm-config`)
and point `LLVM_SYS_181_PREFIX` at the install prefix.

## How it works

`kiln_codegen` lowers MIR to Cranelift IR for both backends. With LLVM,
instead of compiling that IR, it records each function and data object and
hands them to `kiln_llvm`, which translates them into one LLVM module, runs
`default<O3>`, and emits the object file. Both backends therefore share one
lowering of the language: every feature behaves the same, and the
translation only maps about seventy Cranelift instructions.

The translation gives LLVM what it needs to optimize:

- **Pointers.** Cranelift IR has no pointer type. `kiln_llvm` infers which
  values are addresses (stack slots, globals, load/store addresses, and what
  flows into them through address arithmetic, block parameters, and
  `select`) and keeps them as `ptr` with `getelementptr` arithmetic, so SROA,
  `mem2reg`, and alias analysis work. A wrong guess only costs speed:
  `inttoptr`/`ptrtoint` convert at the edges.
- **`noalias` from the borrow rules.** Kiln passes every argument as a
  pointer to a slot the caller fills for that call, so argument slots are
  `noalias`, with `dereferenceable` and `align` from the type's layout. For a
  `&mut T` argument, or a `&T` whose `T` has no interior mutability, the LLVM
  function is a tiny always-inlined thunk that loads the reference and passes
  it to the real body as a `noalias nonnull dereferenceable(size) align(a)`
  parameter (`readonly` for `&T`). This is the same information `rustc` gives
  LLVM, and LLVM's own capture-aware analysis and inliner apply it soundly.
  Atomics and `Once` (which update their fields through `&self`) are
  excluded.
- **Inlining.** Small functions get `inlinehint` (Kiln's own MIR inliner has
  already run); everything is `internal`, so LLVM may also change calling
  conventions and promote arguments.
- **Cold paths and checks.** Panic helpers are `cold noreturn`, and branches
  into blocks that Cranelift marked cold get branch weights. Bounds checks
  that Kiln proved unnecessary are already gone; LLVM removes more.
- **Stack probes.** Every function has `probe-stack="inline-asm"`, so large
  frames touch each page and task stack overflows are caught, as with
  Cranelift.

The target CPU defaults to the baseline (`x86-64`), as `rustc` does;
`KILN_TARGET_CPU=native` uses the host's features. `KILN_LLVM_DUMP=file.ll`
writes the optimized IR, and `KILN_LLVM_VERIFY=1` runs LLVM's verifier
before optimizing.

To check the backend, run the program suite with it:

```sh
KILN_LLVM_VERIFY=1 KILN_RUN_FLAGS="--release --backend=llvm" \
  cargo test --release -p kiln_cli --features llvm --test run
```

(`overflow.kn` expects the debug-build overflow panic, so it differs under
any `--release` run.)
