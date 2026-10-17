# Self-hosting plan

The compiler is written in Rust. The plan is to rewrite it in Kiln one
stage at a time, front to back. Each stage is checked differentially
against the Rust implementation of the same stage before anything
depends on it. The Rust compiler stays the reference (and the bootstrap
compiler) until the Kiln one can build itself.

## Layout

`compiler/` is an ordinary Kiln package (`kilnc`):

- `src/token.kn`: token kinds, keywords, the statement-ending rule.
- `src/lexer.kn`: the lexer (port of `crates/kiln_lexer/src/lexer.rs`).
- `src/main.kn`: the driver. `kilnc <file.kn>...` prints the canonical
  token dump.

Build it with `kiln build` in `compiler/`.

## Differential testing

Each stage gets a canonical text dump implemented on both sides. For the
lexer, that is `kiln_lexer::dump`, printed by `kiln lex --dump <file>`:
one line per token with its byte span and payload, then doc comments,
then errors. `crates/kiln_cli/tests/lexer_diff.rs` builds `kilnc`, runs
it on every `.kn` file in the repository, and requires its output to be
identical. That includes std, examples, tests, benchmarks, the compiler
itself, and the edge-case corpus in `tests/lexer/` (every lexical error
path, all literal forms, CRLF and Unicode whitespace). It runs with
`cargo test`.

## Stages

| Stage | Rust crate | Kiln | Status |
|---|---|---|---|
| 1. Lexer | `kiln_lexer` | `compiler/src/lexer.kn` | ✅ ported; identical on 146 files |
| 2. Parser + AST | `kiln_parser` | `compiler/src/parser.kn` | ⬜ next. Needs a canonical AST dump (`kiln parse --dump`) to diff against |
| 3. Name resolution | `kiln_resolve` | | ⬜ |
| 4. Type checking | `kiln_typeck` | | ⬜ |
| 5. MIR + borrow checking | `kiln_mir`, `kiln_borrowck` | | ⬜ |
| 6. Codegen | `kiln_codegen` | | ⬜. Emit C or LLVM IR text first (no Cranelift bindings from Kiln) |
| 7. Bootstrap | | | ⬜ `kilnc` builds `kilnc`; the stage-2 binary must reproduce stage 1 |

Until stage 6, the Kiln stages feed their output back to the Rust
compiler through dumps only. Nothing in the Rust build depends on
`compiler/`.

## Gaps the lexer port hit

- **u128 is barely usable.** u128 constants fail in codegen ("does not
  support this constant yet"), `{x}` formatting of u128 passes it to an
  i64 runtime function (verifier error), and `num.kn` has no
  `checked_*`/`wrapping_*` for 128-bit types (Cranelift has no 128-bit
  `umulhi`). The port uses a four-limb `U128` instead. *Open.*
- **An assignment couldn't be a match-arm body** (`Some(n) => v = n,`).
  *Fixed:* the parser now reads such an arm as a one-statement block
  (`tests/run/match_assign_arm.kn`).
- **`"}"` and `"{"` in string literals must be written `"}}"` and `"{{"`,**
  because every string literal interpolates. This is by design, but
  surprising when porting Rust code. A plain `"{"` silently starts an
  interpolation (in the punctuation table, it swallowed the following
  `", "` and dropped an element); the type error came from the array
  length. *By design; documented here.*
- **Unicode classes:** Kiln's `char::is_alphabetic` uses letter categories
  (`std::unicode::is_letter`), while Rust's uses the `Alphabetic` property
  (which also covers letter numbers and some combining marks). No file in
  the repository differs. An identifier with such a character would lex
  differently. *Open.*
