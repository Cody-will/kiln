# Benchmarks: Kiln vs Go vs Rust

The same program in three languages, in `bench/<name>/main.{kn,go,rs}`. Run everything with `python3 bench/run.py` (it needs `kiln` built in release mode with the LLVM feature, see `docs/LLVM.md`, plus `go`, `cargo`, `hyperfine`, and `oha`). Raw numbers are in `results.json`.

## Machine and toolchains

| | |
|---|---|
| CPU | Intel Xeon (virtualized), 8 vCPUs, 1 thread per core |
| Memory | 16 GB |
| OS | Linux 6.12 (x86_64) |
| Kiln | commit `bc6a47f`, `kiln build --release` (Cranelift, `opt_level=speed`) |
| Go | go1.24.4, `go build` (default settings) |
| Rust | rustc 1.85.1, `--release` with `opt-level=3`, `codegen-units=1`, thin LTO |

This is a shared cloud VM, so expect some noise (hyperfine reported outliers). Each time is the **median of 5 runs** after 1 warm-up run (`hyperfine -N`). Memory is **peak RSS**, the highest of repeated runs, measured with `maxrss.c` (wait4 rusage from a tiny parent process). All three versions print identical output for every benchmark. The one exception is JSON byte counts, because Go writes `99.0` as `99`; there only the decoded checksum is compared, and it matches.

## Results (lower is better)

Kiln is built twice: **Cranelift** (`--release --backend=cranelift`) and **LLVM** (`--release --backend=llvm`, the `--release` default when the compiler is built with LLVM). **Original** is the first run (`results-before.json`, Cranelift only, before the inlining, bounds-check, LLVM, JSON and runtime work); **previous run** is the first LLVM release, before return values in registers, the number formatting/parsing work and the scheduler fix. Ratios compare Kiln LLVM with Go and Rust.

| Benchmark | Kiln original (s) | Kiln LLVM, previous run (s) | Kiln Cranelift (s) | Kiln LLVM (s) | Go (s) | Rust (s) | LLVM/Go | LLVM/Rust | Peak RSS MiB (Kiln LLVM / Go / Rust) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| fib(35), recursion | 0.124 | 0.039 | 0.077 | **0.002** | 0.041 | 0.019 | 0.06× | 0.12× | 2.0 / 1.7 / 2.1 |
| nbody, 5M steps | 2.965 | 0.219 | 0.671 | **0.206** | 0.231 | 0.159 | 0.89× | 1.30× | 1.9 / 1.8 / 2.0 |
| binary-trees, depth 18 | 3.083 | 1.310 | 2.288 | **1.184** | 1.645 | 0.932 | 0.72× | 1.27× | 50.0 / 40.0 / 34.0 |
| spectral-norm, n=2000 | 2.089 | 0.183 | 0.380 | **0.180** | 0.168 | 0.165 | 1.07× | 1.09× | 2.1 / 1.8 / 2.1 |
| fannkuch-redux, n=10 | 0.880 | 0.218 | 0.330 | **0.218** | 0.175 | 0.180 | 1.24× | 1.21× | 1.9 / 1.8 / 2.0 |
| JSON encode+decode, 50k records ×4 | 4.911 | 0.796 | 0.899 | **0.327** | 0.513 | 0.143 | 0.64× | 2.29× | 48.1 / 75.9 / 45.9 |
| HashMap ops (1M int, 200k string keys) | 0.986 | 0.284 | 0.428 | **0.280** | 0.260 | 0.238 | 1.08× | 1.17× | 61.3 / 67.8 / 53.2 |
| channel ping-pong, 500k round trips | 2.855 | 0.113 | 0.110 | **0.111** | 0.141 | 3.655 | 0.79× | 0.03× | 2.0 / 1.7 / 2.1 |
| worker pool, 8 workers, 200k jobs | 0.122 | 0.131 | 0.108 | **0.102** | 0.077 | 0.164 | 1.33× | 0.62× | 2.2 / 1.8 / 2.3 |

### HTTP (higher req/s is better)

`GET /hello` returning a 14-byte body with keep-alive. Load came from `oha` on the same machine: 64 connections, 2 s warm-up, then 10 s measured. Kiln uses `std::net::http`, Go uses `net/http`, and Rust uses axum 0.7 on tokio.

| | req/s | p50 latency | p99 latency | server peak RSS |
|---|---:|---:|---:|---:|
| Kiln original | 141,417 | 0.43 ms | 0.99 ms | 70.2 MiB |
| Kiln LLVM, previous run | 216,061 | 0.27 ms | 0.71 ms | 4.0 MiB |
| Kiln Cranelift | 188,786 | 0.32 ms | 0.74 ms | 7.4 MiB |
| Kiln LLVM | 227,428 | 0.26 ms | 0.69 ms | 4.0 MiB |
| Go | 203,338 | 0.27 ms | 1.14 ms | 14.7 MiB |
| Rust | 219,155 | 0.27 ms | 0.63 ms | 6.9 MiB |

The load generator shares the 8 vCPUs with the server, so these numbers compare the servers against each other. They are not absolute capacity figures.

## Analysis

**With LLVM, Kiln compute is within about 1.0–1.3× of Rust and at or ahead of Go**, except fib and JSON. The LLVM backend translates the same Cranelift IR the debug backend uses, then gives LLVM what it needs: `noalias`/`dereferenceable` on argument slots and on `&mut T` (and on `&T` without interior mutability, marked `readonly`), `inlinehint` on small functions, `cold`/`noreturn` panic paths with branch weights, and the bounds checks the MIR already proves unneeded are gone. See `docs/LLVM.md`.

- **fib (0.002 s, 10× faster than Rust: not a like-for-like number).** Both inputs are opaque (`std::mem::black_box(35)` in Kiln, `std::hint::black_box` in Rust). Internal functions called only directly now return results of up to 16 bytes in registers instead of through a return slot (LLVM's argument promotion already passed arguments in registers). That leaves fib's IR pure and register-only, and LLVM then turns the second recursive call into an accumulator loop and unrolls the recursion into `main`. rustc's IR doesn't get that far. Treat this row as "call overhead is gone", not as a 10× speedup.
- **binary-trees (1.33×)** is `malloc`/`free`-bound. Rust is ahead by its allocator use, not code quality.
- **JSON (2.3× Rust, 0.64× Go; was 31× Rust).** Derived types write and read straight to and from text (`write_json` / `read_json`), now for enums and tuple structs as well as structs, with no `Value` tree. Floats are formatted with Ryu, shortest round-trip (vendored in `crates/kiln_runtime/runtime/ryu`), instead of up to 17 `snprintf`/`strtod` rounds, and numbers are appended in place (`std::strconv::append_int` / `append_float`) with no temporary string. Parsing takes an exact fast path: integers accumulate while scanning, and decimals with ≤19 digits and exponents within ±22 use a single exact multiply or divide; everything else uses the general parser. Error messages are unchanged: on a type error, decoding re-runs the tree path. What remains is serde_json's hand-tuned scanning and Kiln's per-field `string` allocations.
- **Cranelift release** is still 1.5–3× behind LLVM on loops. It remains the fast-compile default for debug builds and the fallback when LLVM isn't installed.

**Concurrency.** Unbuffered ping-pong is **faster than Go** (0.111 s vs 0.141 s). A task woken by an unbuffered handoff runs next on the same worker (Go's `runnext`), without a futex wake-up, and a streak limit keeps a ping-pong pair from starving the run queue. The worker pool went from 0.131 s to **0.102 s** (1.3× Go, 0.6× Rust). The fix was to stop using `runnext` for *buffered* channels: a blocked producer woken by a receive was parked behind the receiving worker's current task, where idle workers couldn't steal it. Spin locks and spinning before parking were measured and rejected (more CPU, slower). What's left is futex sleep and wake-up of idle workers (high sys time).

**HTTP: 112% of Go's throughput and 104% of Rust's, with the lowest memory of the five (4.0 MiB).** The old 70 MiB was not stacks. Each socket read with a timeout pushed a timer-heap entry that stayed until its deadline, so the heap grew with request rate. Each socket direction now keeps at most one pending timer that re-arms itself at the latest deadline. Task stacks were already reserved lazily (8 MiB address space, committed on touch). Now a stack that ran deep is trimmed back to 16 KiB (`madvise`) before it is reused, and the pool holds 256 stacks. The load generator shares the CPU, so treat Kiln, Go and Rust as roughly equal here.

**Memory** for small programs is about 2 MiB in all three. Kiln uses less than Go for JSON, HashMap and HTTP.

### What would move these numbers next

1. Register passing in the Cranelift backend too (it still uses stack slots; LLVM recovers registers on its own).
2. Fewer allocations in JSON decoding: reuse key and string buffers.
3. Idle-worker parking without a futex per wake-up (a spinning worker handshake that doesn't burn CPU), for the worker pool.
