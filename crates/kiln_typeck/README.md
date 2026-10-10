# kiln_typeck

Type checking for Kiln.

```rust
let diagnostics = kiln_typeck::check_source(src); // parse + resolve + check
```

It checks:

- type inference within functions, with integer and float literals defaulting to `i32` and `f64`
- structs, enums, tuple structs, generics, and generic arity
- traits and impls: missing or extra methods, signature mismatches, default methods, bounds at call sites, `#[derive]`, and `dyn Trait`
- `Result`/`Option` with `?`, including `From` conversion of error types
- explicit `return` (SPEC §3.1): non-unit functions must return on every path
- `if`/`match` as values, and match exhaustiveness for enums and `bool`
- `:=` (tuple destructuring, no unit bindings) and mutability of assignments and `&mut` borrows
- channels: `tx <- v`, `<-rx`, `select`, `chan::<T>(n)`, and direction (`chan<- T` vs `<-chan T`)

It does not yet check lifetimes or borrows, or the types of `std` items (which are opaque). See [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) for the full gap list.

Used by `kiln check <file.kn>`.
