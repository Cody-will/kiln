# Conformance suite

One directory per section of [`docs/SPEC.md`](../../docs/SPEC.md):

| Directory | SPEC section |
|---|---|
| `02-lexical` | §2 Lexical Structure |
| `03-statements` | §3 Statements, Declarations, and Returns |
| `04-examples` | §4 Examples (traits, errors, concurrency) |
| `05-types` | §5 Type System |
| `06-memory` | §6 Memory Model |
| `07-modules` | §7 Modules, Packages, Build Tool |
| `08-limits` | §2/§7 implementation limits (nesting depth, item placement) |

Each `X.kn` runs and its output is compared with `X.out` (stdout, stderr,
then `[exit N]` for a nonzero exit), exactly like `tests/run`. A program whose
first line is `// expect-error: TEXT` must instead be rejected by `kiln build`
with exit status 1 and TEXT in the diagnostics.

The driver is `conformance_suite` in `crates/kiln_cli/tests/run.rs`:

```sh
cargo test -p kiln_cli --test run conformance_suite
KILN_BLESS=1 cargo test -p kiln_cli --test run conformance_suite   # rewrite .out files
```

Behaviour covered here is part of the 1.0 compatibility promise
(see [`docs/STABILITY.md`](../../docs/STABILITY.md)). The standard library
(§8) is covered by the `std_*` programs in `tests/run`.
