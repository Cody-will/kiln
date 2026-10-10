# Kiln Compiler Architecture

The compiler is a Cargo workspace written in Rust. This document describes what exists today and where each planned phase will live.

## Pipeline

```
source (.kn)
  │  kiln_lexer        tokens + doc comments + diagnostics
  ▼
tokens
  │  kiln_parser       AST (+ diagnostics, error recovery)
  ▼
AST
  │  kiln_resolve      (planned) module tree, imports, name resolution → HIR
  │  kiln_types        (planned) type inference, trait solving
  │  kiln_mir          (planned) MIR lowering: CFG, explicit drops/moves
  │  kiln_borrowck     (planned) NLL borrow checking, missing-return checks
  │  kiln_codegen_llvm (planned) LLVM IR via inkwell, monomorphization
  ▼
native binary + kiln_runtime (planned M:N scheduler, channels, netpoller)
```

## Crates

### `kiln_lexer`

- `span.rs`:
  - `Span` is a pair of byte offsets.
  - `LineIndex` converts an offset to a line and column.
- `diagnostic.rs`: `Diagnostic` with rustc-style rendering. This is shared by all phases.
- `token.rs`: `TokenKind` and `Keyword`. `TokenKind::ends_statement` defines the semicolon-insertion rule.
- `lexer.rs`: a hand-written, single-pass lexer. Notable behavior:
  - **Semicolon insertion:** at a newline it inserts `Semi { implicit: true }` after a statement-ending token, unless the next line starts with `.`. A block comment that spans lines counts as a newline.
  - **String interpolation:**
    - The lexer splits a string into `StrPart::Text` and `StrPart::Interp { src, offset }`.
    - It does not lex the interpolated expression itself. The parser re-lexes `src` with `lex_at(src, offset)`, so spans still point into the original file.
    - Braces are written `{{` and `}}`.
  - **Lifetimes and chars:** `'a` is a lifetime and `'a'` is a char. The deciding factor is whether a closing quote follows the identifier characters.
  - **Number literals:** `1..2` and `5.secs()` are not floats, because a float needs a digit after the `.`.
  - **Error recovery:** errors are collected and lexing continues.

### `kiln_parser`

- **`ast.rs`:** the syntax tree.
  - Every node carries a `Span`.
  - A block is a flat list of statements. Whether a block yields a value (only `if`/`match` arms in value position, per SPEC §3.2) is decided later, not by the parser.
- **`parser.rs`:**
  - Items, statements, and patterns are parsed by recursive descent.
  - Expressions use Pratt-style binding powers: ranges < `||` < `&&` < comparisons < `|` < `^` < `&` < shifts < `+ -` < `* / %` < `as` < unary < postfix.
  - Key techniques:
    - **The `no_struct` flag:** inside `if`/`while`/`for`/`match`/`select` heads, `Path {` is not a struct literal. This is the same rule Rust uses.
    - **`>>` splitting:** `expect_gt` splits `>>`, `>=`, and `>>=` so that nested generics close correctly.
    - **Short-declaration lookahead:** `at_short_decl` peeks ahead for `x [mut] :=` and `(a, b mut) :=`.
    - **Newline-tolerant lists:** `comma_list` and `brace_list` ignore implicit semicolons between elements.
    - **Recovery:** `recover_item` skips to the next item keyword at depth 0. `recover_stmt` skips to the next statement boundary.
- **`lib.rs`:** the public API, `parse(src) -> Parse { file, errors }` and `parse_expr`.

### `kiln_cli`

This builds the `kiln` binary. Today it has the `lex` and `parse` subcommands. `build`, `run`, `fmt`, and `test` will be added as the matching phases land.

## Design principles for the codebase

- **No external dependencies in the front end.** This keeps builds fast and makes it easy to embed the front end in the language server.
- **Spans everywhere.** Every diagnostic points at a byte range.
- **Recover, don't stop.** Each phase reports as many errors as it reliably can.
- **One engine for the compiler and the IDE.** Later phases will move to a query-based design (salsa-style) so that `kiln-ls` reuses the compiler directly.

## Planned layout (from SPEC §11)

```
crates/
  kiln_lexer  kiln_parser  kiln_resolve  kiln_types  kiln_mir  kiln_borrowck
  kiln_codegen_llvm  kiln_runtime  kiln_fmt  kiln_pkg  kiln_ls  kiln_cli
std/                     the standard library, in Kiln
editors/tree-sitter-kiln/  editors/vscode-kiln/
```

The AST currently lives in `kiln_parser::ast`. It will move to its own crate if a lossless CST (needed by the formatter and language server) is introduced.
