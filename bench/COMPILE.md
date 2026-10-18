# Compile-time benchmarks

Run with `python3 bench/compile/run.py --runs 3`. The script lives in bench/compile/, and gen.py generates the ~50k-line program in Kiln, Go and Rust. Times are wall-clock medians of 3 runs on 8 cores. A clean build has empty caches: Go gets a fresh std-only GOCACHE, and cargo runs `clean -p`. An incremental build is a one-line edit to one module.

| Compiler | hello clean | hello incr. | mid clean | mid incr. | big clean | big incr. |
|---|---:|---:|---:|---:|---:|---:|
| kiln (Cranelift, debug) | 0.25 s | 0.25 s | 0.42 s | 0.41 s | 4.46 s | 4.50 s |
| kiln (LLVM, release) | 0.27 s | 0.25 s | 1.23 s | 1.28 s | 18.36 s | 18.63 s |
| kilnc (debug) | 0.19 s | 0.19 s | 0.39 s | 0.36 s | 3.29 s | 2.52 s |
| kilnc (release) | 0.24 s | 0.23 s | 1.04 s | 1.12 s | 13.18 s | 11.87 s |
| go build | 0.14 s | 0.13 s | 0.16 s | 0.17 s | 1.22 s | 1.33 s |
| cargo build (debug) | 0.20 s | 0.19 s | 0.45 s | 0.29 s | 5.30 s | 1.27 s |
| cargo build (release) | 0.12 s | 0.12 s | 0.72 s | 0.73 s | 7.48 s | 7.42 s |

The mid program is bench/json. The big program is 50 modules, 50,505 lines.

## Where the time goes (`kilnc --time-passes`, big, debug)

parse 0.2 s · resolve 0.12 s · typeck 0.2–0.4 s · borrowck 0.65 s · codegen 0.75 s · write IR 0.1–0.5 s · clang 0.9 s (0.28 s incremental).

## Fixes made
- **Debug builds:** clang -O1 on one module (34 s) became sharded `-O0` modules compiled in parallel (0.9 s). An unchanged shard keeps its `.o`.
- **Lazy std typeck:** std function bodies are checked only when lowering reaches them. Hello world dropped from 0.43 s to 0.17 s.
- **Body reuse:** the bodies lowered for borrowck are reused by codegen instead of being lowered twice.
- **Release builds:** ThinLTO shards linked with lld (used when clang can find `ld.lld`). The big build dropped from 33 s to 13 s.

## Remaining gap
Against Go, the remaining gap is the front end (about 2 s on big). Profiles show malloc/free, vec growth and clone/drop glue. Borrowck's dataflow is the largest single pass. Next steps: arena allocation and interning, and per-module caching of front-end results.
