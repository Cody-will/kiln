# kiln_mir

Kiln's mid-level IR: each function body (and each closure and `go` block)
lowered from the typed AST into a control-flow graph of basic blocks.

MIR makes explicit what the surface syntax leaves implicit:

- **Evaluation order**, with temporaries for intermediate values.
- **Moves vs copies**: an operand is `move place` for non-`Copy` types and
  `copy place` for `Copy` types and non-consuming reads (comparisons, printing,
  match scrutinees).
- **Borrows**: `&`/`&mut`, including autoref of method receivers (after the
  arguments are evaluated: two-phase borrows) and reborrows of `&mut` arguments.
- **Places**: a local plus field / deref / index projections, with autoderef
  and match ergonomics already applied.
- **Scope ends**: `StorageDead` in reverse declaration order.
- **`defer`**: lowered once into the function's exit block, before locals die.
- **Borrow flow through calls**: each call records which arguments its result
  may borrow from, using the callee's signature and lifetime elision (`sig.rs`).
- **Tasks**: `Spawn` statements for `go f(..)`, `go { }`, and `Scope::spawn`,
  with inferred captures.

It is consumed by `kiln_borrowck` today and will feed code generation.

```sh
kiln mir examples/hello.kn   # print the MIR
```

Modules: `ir.rs` (data structures), `lower.rs` (lowering), `sig.rs`
(signature lifetimes and elision), `pretty.rs` (printer).
