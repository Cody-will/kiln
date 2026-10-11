# Benchmarks: Kiln vs Go vs Rust

The same program in three languages, in `bench/<name>/main.{kn,go,rs}`. Run everything with `python3 bench/run.py` (it needs `kiln` built in release mode, plus `go`, `cargo`, `hyperfine`, and `oha`). Raw numbers are in `results.json`.

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

| Benchmark | Kiln (s) | Go (s) | Rust (s) | Kiln/Go | Kiln/Rust | Peak RSS MiB (Kiln / Go / Rust) |
|---|---:|---:|---:|---:|---:|---|
| fib(35), recursion | 0.124 | 0.048 | 0.022 | 2.6× | 5.7× | 2.1 / 1.8 / 2.0 |
| nbody, 5M steps | 2.965 | 0.266 | 0.190 | 11.2× | 15.6× | 2.1 / 1.8 / 1.9 |
| binary-trees, depth 18 | 3.083 | 1.728 | 1.032 | 1.8× | 3.0× | 50.1 / 36.9 / 34.1 |
| spectral-norm, n=2000 | 2.089 | 0.199 | 0.182 | 10.5× | 11.5× | 2.1 / 1.8 / 2.2 |
| fannkuch-redux, n=10 | 0.880 | 0.205 | 0.202 | 4.3× | 4.4× | 2.0 / 1.8 / 2.0 |
| JSON encode+decode, 50k records ×4 | 4.911 | 0.574 | 0.156 | 8.6× | 31.4× | 176.6 / 74.8 / 45.9 |
| HashMap ops (1M int, 200k string keys) | 0.986 | 0.275 | 0.270 | 3.6× | 3.7× | 61.3 / 66.5 / 53.1 |
| channel ping-pong, 500k round trips | 2.855 | 0.162 | 4.123 | 17.7× | **0.7×** | 2.1 / 1.8 / 2.0 |
| worker pool, 8 workers, 200k jobs | 0.122 | 0.083 | 0.172 | 1.5× | **0.7×** | 2.1 / 1.8 / 2.3 |

### HTTP (higher req/s is better)

`GET /hello` returning a 14-byte body with keep-alive. Load came from `oha` on the same machine: 64 connections, 2 s warm-up, then 10 s measured. Kiln uses `std::net::http`, Go uses `net/http`, and Rust uses axum 0.7 on tokio.

| | req/s | p50 latency | p99 latency | server peak RSS |
|---|---:|---:|---:|---:|
| Kiln | 141,417 | 0.43 ms | 0.99 ms | 70.2 MiB |
| Go | 191,157 | 0.28 ms | 1.29 ms | 15.2 MiB |
| Rust | 210,147 | 0.28 ms | 0.71 ms | 6.6 MiB |

The load generator shares the 8 vCPUs with the server, so these numbers compare the three servers against each other. They are not absolute capacity figures.

## Analysis

**Kiln is slower than both, by 2–15× on compute.** Most of that comes from the backend. Kiln compiles with Cranelift, which aims for fast compilation and does little optimization. It does no inlining across functions, no loop-invariant code motion, and no vectorization, and its register allocation and instruction selection are simpler than LLVM's (which rustc uses) or Go's SSA backend. The tight floating-point loops show this most:

- **nbody and spectral-norm (10–15×).** Every `b[i].x` goes through a `Vec` index call with a bounds check, and those calls are not inlined. The small `a(i, j)` helper in spectral-norm is also a real call in the inner loop. LLVM inlines all of that, hoists the bounds checks, and keeps values in registers. Go inlines `a` and eliminates many of its bounds checks.
- **fib (2.6× / 5.7×)** measures pure call overhead. Kiln's prologue also checks the preemption and stack budget, as Go's does. Go pays the same kind of cost, so the gap to Go is smaller.
- **fannkuch and hashmap (~4×)** are dominated by `Vec`/`HashMap` method calls that LLVM and Go would inline. Kiln's `HashMap` is written in Kiln itself.
- **binary-trees (1.8× / 3.0×)** is mostly allocation. Kiln uses the system `malloc` with no GC, so it doesn't have Go's GC tuning advantage, but it also doesn't pay for a GC. It is the closest compute result.
- **JSON (8.6× / 31×, and 3–4× the memory)** is Kiln's weakest result. `std::encoding::json` builds a full `Value` tree in both directions: encode makes `Value`s and then text, and decode makes a `Value` and then structs. serde_json streams straight between structs and text, and Go's reflection-based encoder also avoids the intermediate tree. A derive that streams directly would fix most of this. It is library work, not a compiler limit.

**Concurrency is where Kiln competes.** Kiln's goroutines and channels beat Rust's standard library: OS threads with `std::sync::mpsc` (ping-pong 0.7×, worker pool 0.7×). The worker pool is within 1.5× of Go, whose scheduler is far more mature. Unbuffered ping-pong is the weak case (17× slower than Go). Every handoff goes through the scheduler's run queue and wakes a worker thread with a futex: sys time (7 s) was higher than user time. Go hands off directly between goroutines on the same P. That is a known runtime improvement (direct handoff and spinning before parking), not a language limit.

**HTTP: 74% of Go's throughput and 67% of Rust's, with good tail latency** (Kiln's p99 beats Go's). Its memory use is higher (70 MiB) because each connection's goroutine reserves an 8 MiB stack. The stacks are committed lazily, but touched pages add up across 64 connections. Smaller initial stacks would close most of that gap.

**Memory** for small programs is the same in all three (about 2 MiB). There's no GC heap overhead, as expected without a GC. HashMap uses less memory than Go's map. Kiln's large numbers come from library choices (the JSON tree, HTTP stacks), not from the ownership model.

### What would move these numbers

1. Inlining small functions in the MIR before codegen (the `Vec` index, `len`, and small helpers): this is the biggest single win for nbody, spectral-norm, fannkuch, and hashmap.
2. Bounds-check elimination for `for i in 0..v.len()` loops.
3. An optional LLVM backend for `--release`, keeping Cranelift for fast debug builds. This is the same split rustc is moving toward with its Cranelift debug backend. It is the route to Rust-level compute performance.
4. Streaming JSON derive (no intermediate `Value`).
5. Scheduler direct handoff for unbuffered channels, and smaller initial goroutine stacks.
