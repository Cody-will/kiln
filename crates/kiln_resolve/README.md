# kiln_resolve

Name resolution for Kiln: builds the module tree, binds `import`s, and resolves every path and local binding in a parsed file.

```rust
let parsed = kiln_parser::parse(src);
let prelude = kiln_resolve::parse_prelude();
let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
for e in &resolved.errors { eprint!("{}", e.render("main.kn", src)); }
```

The result maps path spans to `Res` (item, local, primitive, generic param, `Self`, built-in, or associated item) and binding spans to `LocalId`s. The prelude is written in Kiln, in `src/prelude.kn`.

Diagnostics include undefined names, use before declaration, private items, duplicate definitions, inconsistent or-pattern bindings, unknown loop labels, and type/value namespace mix-ups.

See [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md).
