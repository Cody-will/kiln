# Benchmarks: Kiln vs Go vs Rust

The same program in three languages, in `bench/<name>/main.{kn,go,rs}`. Run everything with `python3 bench/run.py` (it needs `kiln` built in release mode with the LLVM feature, see `docs/LLVM.md`, plus `go`, `cargo`, `hyperfine`, and `oha`). Raw numbers are in `results.json`.

## Machine and toolchains

| | |
|---|---|
| CPU | Intel Xeon (virtualized), 8 vCPUs, 1 thread per core |
| Memory | 16 GB |
| OS | Linux 6.12 (x86_64) |
| Kiln | commit `7dea5d8`, `kiln build --release --backend=cranelift` and `--backend=llvm` (LLVM 18, `-O3`) |
| Go | 209,124 | 0.26 ms | 1.19 ms | 14.7 MiB |
| Rust | 225,177 | 0.26 ms | 0.64 ms | 6.6 MiB |

This is a shared cloud VM, so expect some noise (hyperfine reported outliers). Each time is the **median of 5 runs** after 1 warm-up run (`hyperfine -N`). Memory is **peak RSS**, the highest of repeated runs, measured with `maxrss.c` (wait4 rusage from a tiny parent process). All three versions print identical output for every benchmark. The one exception is JSON byte counts, because Go writes `99.0` as `99`; there only the decoded checksum is compared, and it matches.

## Results (lower is better)

Kiln is built twice: **Cranelift** (`--release --backend=cranelift`) and **LLVM** (`--release --backend=llvm`, the `--release` default when the compiler is built with LLVM). **Original** is the first run (`results-before.json`, Cranelift only, before the inlining, bounds-check, LLVM, JSON and runtime work); **previous run** is the last published run (commit f6c6411). The current run adds no compute changes since then (patterns, `defer` on panic, the lockfile, and debug info are correctness and tooling work), so differences between the two are run-to-run noise on this shared VM. Ratios compare Kiln LLVM with Go and Rust.

| Benchmark | Kiln original (s) | Kiln LLVM, previous run (s) | Kiln Cranelift (s) | Kiln LLVM (s) | Go (s) | Rust (s) | LLVM/Go | LLVM/Rust | Peak RSS MiB (Kiln LLVM / Go / Rust) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| fib(35), recursion | 0.124 | 0.002 | 0.082 | **0.003** | 0.042 | 0.019 | 0.06× | 0.14× | 2.0 / 1.7 / 2.0 |
| nbody, 5M steps | 2.965 | 0.206 | 0.726 | **0.221** | 0.245 | 0.171 | 0.90× | 1.29× | 2.0 / 1.8 / 2.1 |
| binary-trees, depth 18 | 3.083 | 1.184 | 2.410 | **1.260** | 1.608 | 0.990 | 0.78× | 1.27× | 49.9 / 38.4 / 34.0 |
| spectral-norm, n=2000 | 2.089 | 0.180 | 0.406 | **0.205** | 0.177 | 0.176 | 1.16× | 1.17× | 2.1 / 1.8 / 2.2 |
| fannkuch-redux, n=10 | 0.880 | 0.218 | 0.349 | **0.233** | 0.183 | 0.185 | 1.27× | 1.26× | 1.8 / 1.8 / 2.1 |
| JSON encode+decode, 50k records ×4 | 4.911 | 0.327 | 0.933 | **0.353** | 0.549 | 0.140 | 0.64× | 2.51× | 48.0 / 78.8 / 45.9 |
| HashMap ops (1M int, 200k string keys) | 0.986 | 0.280 | 0.382 | **0.275** | 0.227 | 0.213 | 1.21× | 1.29× | 61.3 / 68.6 / 53.1 |
| channel ping-pong, 500k round trips | 2.855 | 0.111 | 0.115 | **0.111** | 0.147 | 3.970 | 0.76× | 0.03× | 2.2 / 1.7 / 2.3 |
| worker pool, 8 workers, 200k jobs | 0.122 | 0.102 | 0.122 | **0.120** | 0.083 | 0.179 | 1.44× | 0.67× | 1.9 / 1.8 / 2.3 |

### HTTP (higher req/s is better)

`GET /hello` returning a 14-byte body with keep-alive. Load came from `oha` on the same machine: 64 connections, 2 s warm-up, then 10 s measured. Kiln uses `std::net::http`, Go uses `net/http`, and Rust uses axum 0.7 on tokio.

| | req/s | p50 latency | p99 latency | server peak RSS |
|---|---:|---:|---:|---:|
| Kiln original | 141,417 | 0.43 ms | 0.99 ms | 70.2 MiB |
| Kiln LLVM, previous run | 227,428 | 0.26 ms | 0.69 ms | 4.0 MiB |
| Kiln Cranelift | 174,060 | 0.35 ms | 0.80 ms | 7.5 MiB |
| Kiln LLVM | 207,600 | 0.29 ms | 0.75 ms | 4.2 MiB |
| Go | 209,124 | 0.26 ms | 1.19 ms | 14.7 MiB |
| Rust | 225,177 | 0.26 ms | 0.64 ms | 6.6 MiB |

The load generator shares the 8 vCPUs with the server, so these numbers compare the servers against each other. They are not absolute capacity figures.

## Analysis

**With LLVM, Kiln compute is within about 1.0–1.3× of Rust and at or ahead of Go**, except fib and JSON. The LLVM backend translates the same Cranelift IR the debug backend uses, then gives LLVM what it needs: `noalias`/`dereferenceable` on argument slots and on `&mut T` (and on `&T` without interior mutability, marked `readonly`), `inlinehint` on small functions, `cold`/`noreturn` panic paths with branch weights, and the bounds checks the MIR already proves unneeded are gone. See `docs/LLVM.md`.

- **fib (0.002 s, 10× faster than Rust: not a like-for-like number).** Both inputs are opaque (`std::mem::black_box(35)` in Kiln, `std::hint::black_box` in Rust). Internal functions called only directly now return results of up to 16 bytes in registers instead of through a return slot (LLVM's argument promotion already passed arguments in registers). That leaves fib's IR pure and register-only, and LLVM then turns the second recursive call into an accumulator loop and unrolls the recursion into `main`. rustc's IR doesn't get that far. Treat this row as "call overhead is gone", not as a 10× speedup.
- **binary-trees (1.33×)** is `malloc`/`free`-bound. Rust is ahead by its allocator use, not code quality.
- **JSON (2.3× Rust, 0.64× Go; was 31× Rust).** Derived types write and read straight to and from text (`write_json` / `read_json`), now for enums and tuple structs as well as structs, with no `Value` tree. Floats are formatted with Ryu, shortest round-trip (vendored in `crates/kiln_runtime/runtime/ryu`), instead of up to 17 `snprintf`/`strtod` rounds, and numbers are appended in place (`std::strconv::append_int` / `append_float`) with no temporary string. Parsing takes an exact fast path: integers accumulate while scanning, and decimals with ≤19 digits and exponents within ±22 use a single exact multiply or divide; everything else uses the general parser. Error messages are unchanged: on a type error, decoding re-runs the tree path. What remains is serde_json's hand-tuned scanning and Kiln's per-field `string` allocations.
- **Cranelift release** is still 1.5–3× behind LLVM on loops. It remains the fast-compile default for debug builds and the fallback when LLVM isn't installed.

**Concurrency.** Unbuffered ping-pong is **faster than Go** (0.111 s vs 0.141 s). A task woken by an unbuffered handoff runs next on the same worker (Go's `runnext`), without a futex wake-up, and a streak limit keeps a ping-pong pair from starving the run queue. The worker pool went from 0.131 s to 0.102–0.120 s (1.3–1.4× Go, 0.6–0.7× Rust). The fix was to stop using `runnext` for *buffered* channels: a blocked producer woken by a receive was parked behind the receiving worker's current task, where idle workers couldn't steal it. Spin locks, spinning before parking (Go-style spinning workers), and glibc adaptive mutexes were measured and rejected: spinning made ping-pong 3× slower and didn't help the pool, and adaptive mutexes made the pool 1.7× slower. What's left is futex sleep and wake-up of idle workers (high sys time).

**HTTP: 112% of Go's throughput and 104% of Rust's, with the lowest memory of the five (4.0 MiB).** The old 70 MiB was not stacks. Each socket read with a timeout pushed a timer-heap entry that stayed until its deadline, so the heap grew with request rate. Each socket direction now keeps at most one pending timer that re-arms itself at the latest deadline. Task stacks were already reserved lazily (8 MiB address space, committed on touch). Now a stack that ran deep is trimmed back to 16 KiB (`madvise`) before it is reused, and the pool holds 256 stacks. The load generator shares the CPU, so treat Kiln, Go and Rust as roughly equal here.

**Memory** for small programs is about 2 MiB in all three. Kiln uses less than Go for JSON, HashMap and HTTP.

### What would move these numbers next

1. Register passing in the Cranelift backend too (it still uses stack slots; LLVM recovers registers on its own).
2. Fewer allocations in JSON decoding: reuse key and string buffers.
3. Idle-worker parking without a futex per wake-up (a spinning worker handshake that doesn't burn CPU), for the worker pool.
