# Compile-time benchmarks

Run with `python3 bench/compile/run.py --runs 3`. The script lives in bench/compile/, and gen.py generates the ~50k-line program in Kiln, Go and Rust. Times are wall-clock medians of 3 runs on 8 cores. A clean build has empty caches: Go gets a fresh std-only GOCACHE, and cargo runs `clean -p`. An incremental build is a one-line edit to one module.

| Compiler | hello clean | hello incr. | mid clean | mid incr. | big clean | big incr. |
|---|---:|---:|---:|---:|---:|---:|
| kiln (Cranelift, debug) | 0.35 s | 0.33 s | 0.53 s | 0.53 s | 4.51 s | 4.41 s |
| kiln (LLVM, release) | 0.35 s | 0.33 s | 1.40 s | 1.37 s | 18.51 s | 19.01 s |
| kilnc (debug) | 0.15 s | 0.15 s | 0.27 s | 0.28 s | 1.91 s | 1.30 s |
| kilnc (release) | 0.20 s | 0.20 s | 0.86 s | 0.81 s | 10.38 s | 9.24 s |
| kilnc built by kilnc (debug) | 0.18 s | 0.17 s | 0.28 s | 0.27 s | 1.81 s | 1.19 s |
| kilnc built by kilnc (release) | 0.22 s | 0.24 s | 0.88 s | 0.83 s | 10.35 s | 9.17 s |
| go build | 0.13 s | 0.13 s | 0.15 s | 0.15 s | 1.22 s | 1.19 s |
| cargo build (debug) | 0.20 s | 0.20 s | 0.42 s | 0.28 s | 5.71 s | 1.17 s |
| cargo build (release) | 0.13 s | 0.14 s | 0.79 s | 0.75 s | 8.54 s | 8.49 s |

The kilnc rows use a kilnc built by the Rust LLVM backend (`KILN_COMPILER=rust`), as in earlier tables. The "built by kilnc" rows use a release kilnc compiled by kilnc itself; it performs like the Rust-built one, slightly faster on big debug. The kiln, go and cargo rows are unchanged from the previous measurement; only the kilnc rows were re-run. (Note: the kilnc that `check.sh` leaves in compiler/target/release is compiled by the Rust Cranelift backend, from the cranelift test suite, and is about 1.9x slower; don't benchmark with it.)

The kiln rows now measure the built-in Rust compiler. In the previous table a stale kilnc sat next to `kiln`, so `kiln build` handed most builds to it (hello 0.25 s). The built-in compiler type-checks all of std on every build, so hello costs 0.33 s with the old std and 0.38 s with the current one, after the http2, websocket and postgres modules were added.

Debug builds carry full DWARF line tables (`!dbg` on every instruction); none of the speedups below trade them away.

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

## Changes since the last measurement
Big debug clean went from 3.09 s to 1.91 s (1.81 s with the self-built kilnc), and big debug incremental from 1.92 s to 1.30 s.
- **Parallel IR writing:** shard texts are built in parallel tasks, and shard compiles run through one fixed pool of clang processes. The runtime's `Command` uses `posix_spawnp` instead of fork, and debug links use lld when it is available.
- **Overlapping clang with codegen:** in debug builds each 64-function shard is handed to a clang worker (nproc-1 of them) as soon as codegen finishes it, instead of after the whole module is written. `KILNC_NO_STREAM` turns this off.
- **Smaller debug IR:** big's IR went from 60.4 MB to 45.0 MB, and its stack slots from 172K to 102K.
  - Store-to-load forwarding in the emitter.
  - An assignment to a plain local that needs no drop is evaluated straight into it, instead of through a scratch slot and a copy.
  - Each overflow check branches to a one-call cold block (`kn_panic_overflow`) that keeps its own debug location.

## Earlier changes
- **Borrow-check cache key:** the compiler binary is identified by size and modification time instead of reading all 10 MB of it on every build.
- **write-ir:** debug-info numbering jumps between placeholders instead of rebuilding the module line by line, and each shard declares only the functions its code references, not every function in the program.
- **Release:** kilnc's MIR inliner and loop-only preemption checks; big release clean went from 12.77 s to 11.52 s.

## Remaining gap
Big debug clean is 1.9 s against Go's 1.2 s, and big incremental is 1.3 s against 1.2 s. The build is now CPU-bound: what remains is serial codegen (rerun in full because symbol numbering is global), about 5 core-seconds of clang on a clean build, and parsing the 400 KB std on every run (50 ms of hello's 0.2 s). Next: per-function IR caching with stable symbol names, and a serialized parsed std.

## Quick-speed pass (Oct 25, 2026) — compile unchanged

No compile-time change this pass. Release big builds remain ~10 s because of
clang ThinLTO; that is a hard clang/LTO tax once IR is written. Tweaking ThinLTO
job count or shard size was not shown to cut wall time without risking runtime,
so release LTO is left as documented above. Runtime wins landed in `kiln_rt.c`
(small-object freelist) and std/derive JSON paths — see `bench/RESULTS.md`.
