# Changelog

## v0.2.0 (2026-11-01)

### Language and compilers
- kilnc (the self-hosted compiler) is the default for `kiln check`, `test`, `[link]` and LSP diagnostics; the Rust compiler is the bootstrap seed and test reference only.
- Associated types (`type Item`, `Self::Item`, `Item = X` bounds), return-position `impl Trait`.
- Integer ranges in exhaustiveness checking, witness patterns in non-exhaustive errors.
- FFI: variadic C calls, `extern "C"` function pointers, `@export extern "C" fun`; `[link] c-sources` on all platforms.
- Implementation limits: nesting depth 64; `mod` inside a function body is rejected (found by fuzzing).

### Standard library
- `std::template`, `std::compress` (DEFLATE/zlib/gzip, streaming writers), `std::url`, `std::mime`.
- `std::net`: `lookup_host` with timeout/context, `connect_timeout`/`connect_ctx`; TLS: mTLS, OS trust store, peer certificates.
- `std::panic::recover`; `Task.join` returns `Err(Panic)` for a panicked task.

### Tooling
- Package registry: git/file index, `kiln add/update/publish`, semver, sha256 lockfile, offline cache.
- LSP: kilnc-backed diagnostics and navigation, quick fixes, semantic tokens.
- `kiln self update/uninstall`, `kiln completions` (bash/zsh/fish).
- Release binaries for Linux (x86_64, arm64), macOS (x86_64, arm64) and Windows (x86_64); `install.ps1`.
  On Windows, `kiln` uses its built-in code generator for now (kilnc is not shipped there yet).

### Quality
- Conformance suite organized by SPEC section; `kiln-fuzz` mutation fuzzer with a CI smoke run.
- Stability policy (`docs/STABILITY.md`); tutorial chapter on a web service; `examples/todo_api` (HTTP + PostgreSQL + JSON) runs in CI.
