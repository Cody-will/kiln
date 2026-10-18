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

## Self-hosted compiler round (Oct 18, 2026; main at `71b0677`)

One full `bench/run.py` run, in seconds (lower is better), median of 5 runs. The
kilnc column is the self-hosted compiler (`kilnc --build --release`, built by the
Rust LLVM backend). The VM was noisy (hyperfine reported outliers).

| Benchmark | Kiln Cranelift | Kiln LLVM | **kilnc** | Go | Rust | kilnc/Go | kilnc/Rust |
|---|---:|---:|---:|---:|---:|---:|---:|
| fib(35) | 0.060 | 0.003 | **0.004** | 0.043 | 0.022 | 0.09× | 0.18× |
| nbody, 5M steps | 0.724 | 0.233 | **0.227** | 0.271 | 0.182 | 0.84× | 1.25× |
| binary-trees, depth 18 | 2.471 | 1.298 | **1.296** | 1.167 | 1.035 | 1.11× | 1.25× |
| spectral-norm, n=2000 | 0.361 | 0.182 | **0.180** | 0.180 | 0.180 | 1.00× | 1.00× |
| fannkuch-redux, n=10 | 0.328 | 0.203 | **0.185** | 0.189 | 0.191 | 0.98× | 0.97× |
| JSON encode+decode | 0.558 | 0.203 | **0.205** | 0.498 | 0.152 | 0.41× | 1.35× |
| HashMap ops | 0.310 | 0.213 | **0.207** | 0.304 | 0.289 | 0.68× | 0.72× |
| channel ping-pong | 0.112 | 0.106 | **0.109** | 0.158 | 3.853 | 0.69× | 0.03× |
| worker pool | 0.078 | 0.081 | **0.080** | 0.085 | 0.177 | 0.94× | 0.45× |

HTTP req/s: Cranelift 151,098, LLVM 178,532, kilnc 183,122, Go 201,474, Rust 217,184.

Notes:
- **HashMap**: kilnc was 1.14× slower than the Rust LLVM backend. It lowered
  `a && b` in a condition to a bool temporary, which LLVM folded into a
  branchless `or` of two compares in the probe loop; on a 16 MB slot table that
  made the cache misses costlier. Both compilers now lower `&&`/`||` conditions
  to jumps (`71b0677`), and kilnc is level with the LLVM backend.
- kilnc is within 3% of the Rust LLVM backend on every benchmark (fib, at
  3–4 ms, is too short to compare), and ahead on fannkuch and HashMap.

## Gap-fix round (Oct 17, 2026; main at `6b07e5e`)

One full `bench/run.py` run, in seconds (lower is better). The VM was noisy
(hyperfine reported outliers). "Before" is the speed-gap round below.

| Benchmark | Kiln LLVM before | Kiln Cranelift | **Kiln LLVM** | Go | Rust | LLVM/Go | LLVM/Rust |
|---|---:|---:|---:|---:|---:|---:|---:|
| fib(35) | 0.003 | 0.058 | **0.003** | 0.041 | 0.021 | 0.07× | 0.14× |
| nbody, 5M steps | 0.201 | 0.685 | **0.213** | 0.247 | 0.171 | 0.86× | 1.25× |
| binary-trees, depth 18 | 1.169 | 2.362 | **1.248** | 1.214 | 1.034 | 1.03× | 1.21× |
| spectral-norm, n=2000 | 0.170 | 0.364 | **0.177** | 0.182 | 0.180 | 0.97× | 0.98× |
| fannkuch-redux, n=10 | 0.181 | 0.325 | **0.188** | 0.187 | 0.189 | 1.01× | 0.99× |
| JSON encode+decode | 0.194 | 0.561 | **0.194** | 0.494 | 0.148 | 0.39× | 1.31× |
| HashMap ops | 0.194 | 0.286 | **0.204** | 0.247 | 0.273 | 0.83× | 0.75× |
| channel ping-pong | 0.102 | 0.109 | **0.128** | 0.164 | 3.890 | 0.78× | 0.03× |
| worker pool | 0.101 | 0.116 | **0.123** | 0.107 | 0.236 | 1.15× | 0.52× |

HTTP req/s: Cranelift 140,845, LLVM 180,112, Go 202,397, Rust 219,958 (the
load generator shares the CPUs; numbers drifted down for every language).

Most numbers are within the noise of the previous round. Notes:
- **Worker pool**: buffered channels now use a lock-free Vyukov MPMC ring
  (doubled stamps so capacity 1 works), with atomic waiter counts so a
  sender or receiver only takes the park lock when someone is asleep. In a
  quiet A/B run (`hyperfine -N -w2 -r15`) Kiln LLVM took 76.9 ± 2.5 ms
  against Go's 83.3 ± 3.1 ms (0.92×); an earlier A/B gave 75.1 vs 76.9 ms.
  The full-suite run above (1.15×) was taken while the box was loaded.
  The cost is CPU: workers spin before parking, ~560 ms user time against
  ~240 ms before.
- **Ping-pong** uses unbuffered channels (unchanged rendezvous path); its
  A/B time is unchanged (~102 ms). The 0.128 above is noise in that run.
- **nbody**: `for` loops over a constant range with ≤4096 trips are now
  marked bounded and skip the preemption check, and checks are emitted only
  on real back-edges (dominator-based). LLVM now fully unrolls the inner
  pair loop, but this gives no speedup: Rust vectorizes the pair math with
  `sqrtpd`/`divpd`, while our unrolled scalar code spills registers. Closing
  that needs SoA-friendly fixed-size arrays (`[f64; N]`) in the language,
  which is not done yet.

## Speed-gap round (Oct 17, 2026; main after `25e5b93`)

Median of 3 full `bench/run.py` runs, in seconds (lower is better). The
three runs agreed within ±4%. "Before" is the last run at `155b3e8`.

| Benchmark | Kiln LLVM before (`155b3e8`) | Kiln Cranelift | **Kiln LLVM** | Go | Rust | LLVM/Go | LLVM/Rust |
|---|---:|---:|---:|---:|---:|---:|---:|
| fib(35), recursion | 0.003 | 0.064 | **0.003** | 0.041 | 0.020 | 0.07× | 0.15× |
| nbody, 5M steps | 0.221 | 0.657 | **0.201** | 0.236 | 0.166 | 0.85× | 1.21× |
| binary-trees, depth 18 | 1.260 | 2.222 | **1.169** | 1.479 | 0.940 | 0.79× | 1.24× |
| spectral-norm, n=2000 | 0.205 | 0.338 | **0.170** | 0.169 | 0.168 | 1.01× | 1.01× |
| fannkuch-redux, n=10 | 0.233 | 0.303 | **0.181** | 0.180 | 0.179 | 1.01× | 1.01× |
| JSON encode+decode, 50k records ×4 | 0.353 | 0.546 | **0.194** | 0.503 | 0.136 | 0.39× | 1.43× |
| HashMap ops (1M int, 200k string keys) | 0.275 | 0.287 | **0.194** | 0.227 | 0.201 | 0.85× | 0.97× |
| channel ping-pong, 500k round trips | 0.111 | 0.104 | **0.102** | 0.143 | 3.624 | 0.71× | 0.03× |
| worker pool, 8 workers, 200k jobs | 0.120 | 0.108 | **0.101** | 0.078 | 0.163 | 1.29× | 0.62× |

HTTP (higher is better). The load generator shares the 8 vCPUs, so
these numbers vary about ±5% from run to run:

| | req/s (median of 3) |
|---|---:|
| Kiln LLVM before | 207,600 |
| kiln-cranelift | 167,349 |
| kiln-llvm | 201,263 |
| go | 223,856 |
| rust | 237,877 |

What changed:
- **Worker pool**: each idle worker now parks on its own condition
  variable (from an idle list), and only one "searching" worker is woken
  at a time. Linux uses a futex lock with a short spin. 120 → ~102 ms.
  The rest of the gap to Go (~1.3×) is contention on the two shared
  channels' mutex: ten tasks hit one lock. A lock-free channel buffer
  would be the next step. Ping-pong did not regress (111 → ~102 ms).
- **JSON**: escaping copies unescaped byte runs in one push, `push_char`
  has an ASCII fast path, bool/f64/i64 are read without building a
  `Value`, and floats that are exact short decimals skip Ryu. `Vec::push`
  calls the runtime only when full, and big freed blocks stay in the heap
  (`mallopt`). 2.5× → 1.43× Rust.
- **Compute**: innermost loops nested in another loop skip the
  preemption countdown, found by a natural-loop analysis. The countdown
  blocked unrolling. Runtime Vec/string functions are declared to LLVM
  with `ptr nocapture` parameters. Spectral-norm and fannkuch now match
  Rust. nbody stays ~1.25× because Rust's `vec![...]` has a
  compile-time length, so LLVM unrolls the 5-body loop fully with
  `sqrtpd`. A Kiln `Vec` built by `push` has no such constant length.
- **HashMap**: index slots carry 24 hash bits, so probes rarely load
  non-matching entries. Now slightly faster than Rust's SipHash map.
- **Cranelift**: direct calls pass scalar arguments and results in
  registers (`Abi::Reg`). Gains: fib −11%, binary-trees −5%,
  fannkuch −6%, json −3%. Spectral-norm got 8% slower.

## Previous run (`7dea5d8`)

 (lower is better)

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

## Fixed-size arrays (nbody on `[Body; 5]`)

nbody now holds its bodies in a `[Body; 5]` passed as `&[Body; 5]` /
`&mut [Body; 5]`. hyperfine (-w1 -r5), same box:

| Version | Kiln LLVM | Rust |
|---|---:|---:|
| `Vec<Body>` (before) | 0.212 | 0.174 |
| `[Body; 5]` | 0.216–0.221 | 0.174–0.177 |

Output is unchanged. With N constant, LLVM removes every bounds check in
`advance` and fully unrolls it, and SLP packs the x/y pairs into
`<2 x double>`. It still does not pair up the `sqrt`/`div` of two different
body pairs the way Rust does (`sqrtpd`/`divpd`), so the result is ~1.25×
Rust, the same as before. A second SLP round after O3 does pack those
(`opt -O3` on the dumped IR gives `llvm.sqrt.v2f64`), but it measured no
faster (221 ms), so it was not kept. Next to try: noalias/escape (the
`energy(&b)` call stops SROA from promoting the array to registers).

## kilnc release round (Oct 18, 2026)

`kilnc --build --release` is the self-hosted compiler: ThinLTO at `-O3`. It now passes Kiln's guarantees to LLVM the way the Rust LLVM backend does:
- Addresses are typed `ptr` (slots, references, Box), with GEP address arithmetic and `inbounds` after bounds checks.
- A register ABI: a `.v` body takes small and fat-reference arguments by value and returns small results by value, behind an always-inlined slot wrapper.
- `noalias` on argument slots, on `&mut T`, and on `&T` of freeze types.
- `nounwind` on every function and `inlinehint` on small ones.
- `cold noreturn` on panics and `nocapture` on runtime pointer parameters.

One `bench/run.py` run, median of 5, in seconds:

| Benchmark | Kiln Cranelift | Kiln LLVM | **kilnc** | Go | Rust |
|---|---:|---:|---:|---:|---:|
| fib(35) | 0.057 | 0.003 | **0.021** | 0.041 | 0.021 |
| nbody | 0.671 | 0.216 | **0.211** | 0.249 | 0.171 |
| binary-trees | 2.291 | 1.253 | **1.209** | 0.999 | 0.970 |
| spectral-norm | 0.358 | 0.177 | **0.184** | 0.176 | 0.178 |
| fannkuch-redux | 0.326 | 0.187 | **0.187** | 0.189 | 0.192 |
| JSON | 0.547 | 0.193 | **0.214** | 0.472 | 0.148 |
| hashmap | 0.284 | 0.199 | **0.223** | 0.239 | 0.240 |
| pingpong | 0.111 | 0.107 | **0.108** | 0.158 | 3.903 |
| workerpool | 0.072 | 0.080 | **0.078** | 0.082 | 0.170 |

kilnc matches the Rust LLVM backend within noise, except JSON and hashmap (about 10% behind) and fib. On fib the Rust backend's pipeline folds the recursion further; kilnc matches rustc there.
