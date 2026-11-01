# Stability and Compatibility Policy

This document says what Kiln promises not to break, starting with 1.0.
Before 1.0 (the 0.x series) anything may change between minor releases,
but every breaking change is listed in the release notes.

## Versioning

Kiln follows [Semantic Versioning](https://semver.org/) for the toolchain
(`kiln`, `kilnc`, the runtime, and `std`), released together under one version.

- **Patch** (1.0.x): bug fixes, performance, diagnostics. No new APIs.
- **Minor** (1.x.0): new language features, `std` APIs, and CLI flags that
  are backward compatible.
- **Major** (2.0): may break the guarantees below; announced at least one
  minor release ahead with deprecation warnings where possible.

## What is stable at 1.0

1. **Language.** Every program accepted by Kiln 1.0 that does not depend on
   behaviour listed under "Exceptions" keeps compiling and means the same thing
   in every 1.x release. The reference is `docs/SPEC.md`; the executable
   reference is the conformance suite in `tests/conformance/`, organized by
   SPEC section. A conformance test is only changed to fix a test that
   contradicts the SPEC.
2. **Standard library.** Public items in the `std` modules listed in SPEC
   §8.2 keep their names, signatures, and documented behaviour, except items
   documented as experimental. New modules, functions, methods, trait impls,
   and variants of error-kind enums may be added (match error kinds with a
   `_` arm).
3. **Package format.** `kiln.toml` keys, `kiln.lock` (readers accept every
   older lockfile version), and the registry index format in `docs/REGISTRY.md`.
4. **CLI.** Subcommands, flags, and exit statuses of `kiln build`, `run`,
   `test`, `fmt`, `doc`, `new`, `add`, `publish`, `self`, and `completions`.
   Human-readable output (diagnostics, progress) is not stable.
5. **Formatting.** `kiln fmt` output only changes in minor releases, and
   never so that already-formatted code fails `kiln fmt --check` within a
   patch series.
6. **C ABI.** `extern "C"` and `@export` follow the platform C ABI.

## Exceptions (may change in any minor release)

- Soundness fixes: if the compiler accepted unsound code (a borrow-check or
  type-check hole), it may start rejecting it. Such changes get a release
  note and, when practical, one release of warnings first.
- Items, modules, and flags documented as *experimental* or *unstable*,
  and everything behind `KILN_*` environment variables.
- Exact diagnostic text, inference in ambiguous cases that was arguably a bug,
  lint warnings, and new warnings.
- Performance characteristics, binary size, the layout of types without
  `@repr(C)`, symbol names, and the runtime's scheduling order.
- The deprecated built-in Rust code generator (`KILN_ALLOW_RUST_FALLBACK`).
- Supported platforms below tier 1 (see below).

## Platforms

| Tier | Targets | Promise |
|---|---|---|
| 1 | x86_64/aarch64 Linux (glibc) | Built and fully tested in CI on every commit; release binaries. |
| 2 | x86_64/arm64 macOS, x86_64 Windows | Release binaries built in CI; test suite run when CI capacity allows. |
| 3 | Others (musl, BSDs) | Expected to build from source; no guarantees. |

## Deprecation

A stable item is deprecated for at least one minor release (with a compiler
warning naming the replacement) before it is removed in the next major
release. Removing it in a minor release requires a soundness or security reason.

## Minimum toolchain for building from source

Building `kiln` from source needs the Rust toolchain version given in
`rust-version` in `Cargo.toml` and a C compiler (clang preferred). Raising
either is allowed in a minor release and is noted in the release notes.
