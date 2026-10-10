# kiln_runtime

The runtime library linked into every Kiln executable, written in C (`runtime/kiln_rt.c`).

`build.rs` compiles it with the `cc` crate, and `src/lib.rs` embeds the resulting static library (`kiln_runtime::LIB`, named `kiln_runtime::LIB_NAME`) so the `kiln` binary is self-contained; the linker only needs the system C library.

It provides:

- Memory: `kn_alloc`, `kn_free`, `kn_vec_reserve`
- Panics: `kn_panic` flushes stdout, prints `panic: <message>` to stderr, and exits with status 101
- Output: `kn_print` (stdout or stderr, optional newline)
- Strings: building and formatting (`kn_string_push`, `kn_fmt_i64`, `kn_fmt_f64`, ...), comparison, and the `str` methods (`to_upper`, `trim`, `contains`, ...)
- The per-thread `defer` stack used by `DeferDispatch`
- Float helpers (`kn_round`, `kn_powi`)

The concurrency runtime (task scheduler, channels, `select`) is the next milestone and will live here.

Layouts shared with the code generator: `String` and `Vec<T>` are `{ ptr, cap, len }`, and `&str` and `&[T]` are `{ ptr, len }`.
