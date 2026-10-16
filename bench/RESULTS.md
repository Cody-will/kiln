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

Kiln is built twice: **Cranelift** (`--release --backend=cranelift`) and **LLVM** (`--release --backend=llvm`, the `--release` default when the compiler is built with LLVM). **Before** is the previous run (`results-before.json`, Cranelift only, before the inlining, bounds-check, LLVM, JSON and runtime work). Ratios compare Kiln LLVM with Go and Rust.

| Benchmark | Kiln before (s) | Kiln Cranelift (s) | Kiln LLVM (s) | Go (s) | Rust (s) | LLVM/Go | LLVM/Rust | Peak RSS MiB (Kiln before → LLVM / Go / Rust) |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| fib(35), recursion | 0.124 | 0.080 | **0.039** | 0.042 | 0.019 | 0.92× | 2.00× | 2.1 → 1.9 / 1.7 / 2.0 |
| nbody, 5M steps | 2.965 | 0.712 | **0.219** | 0.254 | 0.166 | 0.86× | 1.32× | 2.1 → 1.9 / 1.8 / 2.1 |
| binary-trees, depth 18 | 3.083 | 2.418 | **1.310** | 1.535 | 0.988 | 0.85× | 1.33× | 50.1 → 50.0 / 38.1 / 34.0 |
| spectral-norm, n=2000 | 2.089 | 0.392 | **0.183** | 0.180 | 0.174 | 1.02× | 1.06× | 2.1 → 2.0 / 1.8 / 2.2 |
| fannkuch-redux, n=10 | 0.880 | 0.333 | **0.218** | 0.180 | 0.179 | 1.21× | 1.22× | 2.0 → 2.0 / 1.8 / 2.1 |
| JSON encode+decode, 50k records ×4 | 4.911 | 1.340 | **0.796** | 0.560 | 0.147 | 1.42× | 5.41× | 176.6 → 48.2 / 77.5 / 45.8 |
| HashMap ops (1M int, 200k string keys) | 0.986 | 0.387 | **0.284** | 0.258 | 0.231 | 1.10× | 1.23× | 61.3 → 61.3 / 69.0 / 53.2 |
| channel ping-pong, 500k round trips | 2.855 | 0.119 | **0.113** | 0.144 | 3.614 | **0.78×** | **0.03×** | 2.1 → 2.0 / 1.7 / 2.2 |
| worker pool, 8 workers, 200k jobs | 0.122 | 0.126 | **0.131** | 0.079 | 0.171 | 1.65× | **0.76×** | 2.1 → 1.9 / 1.8 / 2.2 |

### HTTP (higher req/s is better)

`GET /hello` returning a 14-byte body with keep-alive. Load came from `oha` on the same machine: 64 connections, 2 s warm-up, then 10 s measured. Kiln uses `std::net::http`, Go uses `net/http`, and Rust uses axum 0.7 on tokio.

| | req/s | p50 latency | p99 latency | server peak RSS |
|---|---:|---:|---:|---:|
| Kiln before | 141,417 | 0.43 ms | 0.99 ms | 70.2 MiB |
| Kiln Cranelift | 177,714 | 0.34 ms | 0.79 ms | 7.4 MiB |
| Kiln LLVM | **216,061** | 0.27 ms | 0.71 ms | **4.0 MiB** |
| Go | 212,256 | 0.26 ms | 1.16 ms | 14.7 MiB |
| Rust | 225,878 | 0.26 ms | 0.64 ms | 6.9 MiB |

The load generator shares the 8 vCPUs with the server, so these numbers compare the servers against each other. They are not absolute capacity figures.

## Analysis

**With LLVM, Kiln compute is within about 1.0–1.3× of Rust and at or ahead of Go**, except fib and JSON. The LLVM backend translates the same Cranelift IR the debug backend uses, then gives LLVM what it needs: `noalias`/`dereferenceable` on argument slots and on `&mut T` (and on `&T` without interior mutability, marked `readonly`), `inlinehint` on small functions, `cold`/`noreturn` panic paths with branch weights, and the bounds checks the MIR already proves unneeded are gone. See `docs/LLVM.md`.

- **fib (2.0× Rust, faster than Go).** Kiln passes every argument and result through a stack slot (its calling convention). LLVM removes most of that after inlining, but not across a recursive call. Passing scalars in registers is the remaining fix.
- **binary-trees (1.33×)** is `malloc`/`free`-bound. Rust is ahead by its allocator use, not code quality.
- **JSON (5.4× Rust, 1.4× Go; was 31×).** Derived types now write and read straight to and from text (`write_json` / `read_json`) with no `Value` tree, and memory fell from 177 to 48 MiB, below Go. What remains: number formatting and parsing go through general helpers (an allocation per number), and serde_json's hand-tuned scanning. Error messages are unchanged. On a type error, decoding re-runs the tree path to report exactly the previous message.
- **Cranelift release** is still 1.5–3× behind LLVM on loops. It remains the fast-compile default for debug builds and the fallback when LLVM isn't installed.

**Concurrency.** Unbuffered ping-pong went from 17.7× slower than Go to **faster than Go** (0.113 s vs 0.144 s). A task woken by a channel handoff now runs next on the same worker (Go's `runnext`), without a futex wake-up. A streak limit keeps a ping-pong pair from starving the run queue. The worker pool is unchanged (1.65× Go). It is bound by contention on the shared job channel's lock, which Go's lock-free fast paths avoid.

**HTTP: now on par with Go and 96% of Rust, and the lowest memory of the five (4.0 MiB).** The old 70 MiB was not stacks. Each socket read with a timeout pushed a timer-heap entry that stayed until its deadline, so the heap grew with request rate. Each socket direction now keeps at most one pending timer that re-arms itself at the latest deadline. Task stacks were already reserved lazily (8 MiB address space, committed on touch). Now a stack that ran deep is trimmed back to 16 KiB (`madvise`) before it is reused, and the pool holds 256 stacks.

**Memory** for small programs is about 2 MiB in all three. Kiln uses less than Go for JSON, HashMap and HTTP.

### What would move these numbers next

1. Scalar arguments and results in registers (not stack slots): fib and call-heavy code.
2. Faster integer and float formatting and parsing without temporary strings: JSON.
3. Lock-free fast paths for buffered channels: the worker pool.
