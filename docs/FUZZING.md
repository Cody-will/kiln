# Fuzzing

`crates/kiln_fuzz` is a dependency-free mutation fuzzer (cargo-fuzz needs a
nightly toolchain; this runs on stable). It mutates the `tests/` and
`examples/` corpus (byte flips, deletes, token inserts, splices, deep
nesting) and reports any input that makes the front end crash.

```sh
cargo build --release -p kiln_fuzz
# in-process Rust front end: lexer, parser, fmt, resolve + typeck + borrowck
target/release/kiln-fuzz --secs 300 --seed 7
# self-hosted compiler (kilnc --check) as a subprocess; run from repo root
target/release/kiln-fuzz --target kilnc --secs 300 --seed 7
# reproduce a saved crash
target/release/kiln-fuzz --target kilnc --replay target/fuzz-crashes/crash-0.kn
```

A crash is a panic, an exit status other than 0/1, or a 10 s timeout. Crashes
go to `--out` (default `target/fuzz-crashes`). `KILN_FUZZ_VERBOSE=1` keeps
panic output. CI runs a 2000-input smoke session with a fixed seed.

Found and fixed so far: stack overflow on deep nesting (now a 64-level
limit) and a resolver panic on `mod` inside a function body (now rejected).
