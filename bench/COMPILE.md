# Compile-time benchmarks

Run with `python3 bench/compile/run.py --runs 3`. The script lives in bench/compile/, and gen.py generates the ~50k-line program in Kiln, Go and Rust. Times are wall-clock medians of 3 runs on 8 cores. A clean build has empty caches: Go gets a fresh std-only GOCACHE, and cargo runs `clean -p`. An incremental build is a one-line edit to one module.

| Compiler | hello clean | hello incr. | mid clean | mid incr. | big clean | big incr. |
|---|---:|---:|---:|---:|---:|---:|
| kiln (Cranelift, debug) | 0.25 s | 0.25 s | 0.42 s | 0.41 s | 4.46 s | 4.50 s |
| kiln (LLVM, release) | 0.27 s | 0.25 s | 1.23 s | 1.28 s | 18.36 s | 18.63 s |
| kilnc (debug) | 0.20 s | 0.20 s | 0.39 s | 0.37 s | 2.88 s | 1.66 s |
| kilnc (release) | 0.24 s | 0.25 s | 1.03 s | 1.00 s | 12.77 s | 11.24 s |
| go build | 0.14 s | 0.13 s | 0.16 s | 0.17 s | 1.22 s | 1.33 s |
| cargo build (debug) | 0.20 s | 0.19 s | 0.45 s | 0.29 s | 5.30 s | 1.27 s |
| cargo build (release) | 0.12 s | 0.12 s | 0.72 s | 0.73 s | 7.48 s | 7.42 s |

The mid program is bench/json. The big program is 50 modules, 50,505 lines.

## Where the time goes (`kilnc --time-passes`, big, debug)

| pass | clean | incremental |
|---|---:|---:|
| parse | 0.18 s | 0.18 s |
| resolve | 0.10 s | 0.10 s |
| typeck | 0.15 s | 0.15 s |
| borrowck (lowering 0.14 s + checks) | 0.54 s | 0.14 s |
| codegen | 0.56 s | 0.56 s |
| write IR | 0.15 s | 0.15 s |
| clang | 1.1 s | 0.09 s |

Before the allocator and the cache: borrowck 0.66 s, codegen 0.76 s.

## Fixes made
- **Debug builds:** clang -O1 on one module (34 s) became sharded `-O0` modules compiled in parallel (0.9 s). An unchanged shard keeps its `.o`.
- **Lazy std typeck:** std function bodies are checked only when lowering reaches them. Hello world dropped from 0.43 s to 0.17 s.
- **Body reuse:** the bodies lowered for borrowck are reused by codegen instead of being lowered twice.
- **Release builds:** ThinLTO shards linked with lld (used when clang can find `ld.lld`). The big build dropped from 33 s to 13 s.

- **Allocator:** kilnc links compiler/native/arena.c, which replaces malloc in the kilnc binary only. It uses per-thread size-class free lists carved from bump-allocated 4 MiB chunks, and memory is never returned. glibc malloc/free was about 25% of front-end time. This saves about 0.5 s on big and beats mimalloc (tried with LD_PRELOAD). Compiled programs keep the system malloc.
- **Borrow-check cache:** `--build` writes `<out>.bck`, the keys of the functions that passed. A key hashes the function's body text, the program with all bodies cut out, the std text, and the kilnc binary. A body edit re-checks only that function; any change outside bodies re-checks everything. Lowering still runs because codegen needs the bodies. The checks took 0.41 s of the 0.55 s borrowck pass.

## Remaining gap
Big clean is 2.9 s against Go's 1.2 s, and big incremental is 1.7 s against 1.3 s. What remains is codegen (0.56 s, rerun in full because symbol numbering is global), clang over the whole program on a clean build, and parsing the 400 KB std on every run (50 ms of hello's 0.2 s). Next: per-function IR caching with stable symbol names, and a serialized parsed std.
